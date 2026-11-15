//! Safe wrappers for the Unix process, descriptor and socket calls the domain crates need.
//! Every `unsafe` block they would otherwise write lives here.
use std::{
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::Command,
    time::Duration,
};

/// Makes the spawned child the leader of a new session, and so of its own process group,
/// before it execs. A failed `setsid` fails the spawn rather than leaving the child in the
/// caller's session. Pair with [`terminate_process_group`] and [`kill_process_group`].
pub fn new_session(command: &mut Command) {
    // SAFETY: pre_exec runs after fork and before exec, where only async-signal-safe calls
    // are allowed; the closure calls only setsid and reads errno, with no Rust-managed inputs.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// Sends SIGTERM to the process group `leader` heads. Call it only for an unreaped child
/// started through [`new_session`], so the group cannot belong to someone else. A group
/// that no longer exists is not an error.
pub fn terminate_process_group(leader: u32) -> io::Result<()> {
    signal_process_group(leader, libc::SIGTERM)
}

/// Sends SIGKILL to the process group `leader` heads, under the same contract as
/// [`terminate_process_group`].
pub fn kill_process_group(leader: u32) -> io::Result<()> {
    signal_process_group(leader, libc::SIGKILL)
}

fn signal_process_group(leader: u32, signal: libc::c_int) -> io::Result<()> {
    let group = libc::pid_t::try_from(leader)
        .ok()
        .filter(|group| *group > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid process group"))?;
    // SAFETY: killpg takes plain integers and touches no Rust-managed memory; a positive
    // group id never addresses every process the way 0 or -1 would.
    if unsafe { libc::killpg(group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Adds `O_NONBLOCK` to a descriptor, such as a child's stdio pipe, keeping its other flags.
pub fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let fd = fd.as_raw_fd();
    // SAFETY: fcntl only reads the status flags of this live borrowed descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is live for the borrow; adding O_NONBLOCK preserves its flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The effective user id of this process.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and has no memory-safety preconditions.
    unsafe { libc::geteuid() }
}

/// The session id of process `pid`, or the OS error (for example `ESRCH` once it is gone).
/// Lets callers check that a child really leads its own session after [`new_session`].
pub fn session_id(pid: u32) -> io::Result<u32> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id out of range"))?;
    // SAFETY: getsid only reads kernel process state for the given id; it takes no pointers.
    let sid = unsafe { libc::getsid(pid) };
    if sid < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(sid as u32)
}

/// Starts connecting a nonblocking, close-on-exec Unix stream socket to `path` without
/// waiting for the peer to accept. Poll [`poll_writable`] to learn when it has connected.
pub fn connect_unix_nonblocking(path: &Path) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is a valid initial representation; family and path
    // are set below, and the checked length leaves a terminating NUL in sun_path.
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path is invalid or too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = mem::size_of_val(&address) as u8;
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    // SAFETY: socket creates a fresh descriptor with the specified Unix stream family.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful socket returned a unique owned descriptor; OwnedFd closes it on every error path.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    set_nonblocking(owned.as_fd())?;
    // SAFETY: F_SETFD operates on this live owned descriptor; FD_CLOEXEC keeps unrelated
    // subprocesses from retaining the private in-flight connection.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: address points to initialized sockaddr_un storage for the supplied size;
    // the descriptor is live and nonblocking, so connect cannot stall the caller.
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EAGAIN)) {
            return Err(error);
        }
    }
    Ok(UnixStream::from(owned))
}

/// What a zero-timeout write-readiness poll reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteReadiness {
    /// The descriptor accepts writes (`POLLOUT`).
    pub writable: bool,
    /// The peer hung up or the descriptor is invalid (`POLLHUP` or `POLLNVAL`).
    pub hung_up: bool,
}

/// Checks without waiting whether `fd` is writable. `None` means nothing is ready yet.
pub fn poll_writable(fd: BorrowedFd<'_>) -> io::Result<Option<WriteReadiness>> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: descriptor is one initialized pollfd pointing at a live borrowed descriptor;
    // timeout zero makes this a readiness check, never a blocking wait.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result == 0 {
        return Ok(None);
    }
    Ok(Some(WriteReadiness {
        writable: descriptor.revents & libc::POLLOUT != 0,
        hung_up: descriptor.revents & (libc::POLLHUP | libc::POLLNVAL) != 0,
    }))
}

/// Blocks until a `readable` descriptor has input or hung up, a `writable` one accepts
/// output, or `timeout` passes. A signal can end the wait early, so callers must recheck
/// every descriptor (nonblocking) after it returns rather than trust that one is ready.
pub fn wait_until_ready(
    readable: &[BorrowedFd<'_>],
    writable: &[BorrowedFd<'_>],
    timeout: Duration,
) -> io::Result<()> {
    let watch = |fd: &BorrowedFd<'_>, events| libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    let mut descriptors: Vec<libc::pollfd> = readable
        .iter()
        .map(|fd| watch(fd, libc::POLLIN))
        .chain(writable.iter().map(|fd| watch(fd, libc::POLLOUT)))
        .collect();
    let count = libc::nfds_t::try_from(descriptors.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // Rounded up: a deadline less than a millisecond away must not become a busy spin.
    let millis =
        libc::c_int::try_from(timeout.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX);
    // SAFETY: descriptors is an initialized array of `count` pollfds, each naming a
    // descriptor the caller keeps borrowed, and so open, for the whole call.
    let result = unsafe { libc::poll(descriptors.as_mut_ptr(), count, millis) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

/// A self-pipe that any thread can signal to end a [`wait_until_ready`] that includes
/// [`WakePipe::fd`]. Both ends are nonblocking and close on exec.
pub struct WakePipe {
    reader: io::PipeReader,
    writer: io::PipeWriter,
}

impl WakePipe {
    pub fn new() -> io::Result<Self> {
        let (reader, writer) = io::pipe()?;
        set_nonblocking(reader.as_fd())?;
        set_nonblocking(writer.as_fd())?;
        Ok(Self { reader, writer })
    }

    /// Makes [`Self::fd`] readable. Never blocks: a full pipe is already readable.
    pub fn wake(&self) {
        let _ = (&self.writer).write(&[1]);
    }

    /// The end to wait on.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.reader.as_fd()
    }

    /// Consumes every wake signalled so far.
    pub fn clear(&self) {
        let mut buffer = [0; 64];
        loop {
            match (&self.reader).read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

/// Kernel-reported identity of the process on the other end of a connected Unix socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
}

/// Reads the peer's PID and effective user id from the kernel, never from anything the
/// peer wrote. Unsupported outside macOS and Linux.
pub fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut length = mem::size_of_val(&pid) as libc::socklen_t;
        // SAFETY: pid and length are initialized writable storage of the advertised sizes;
        // getsockopt reads kernel credentials from this live connected Unix socket.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize != mem::size_of_val(&pid) || pid <= 0 {
            return Err(io::Error::other("Kernel returned no peer PID"));
        }
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: uid/gid are valid writable scalar outputs for this live socket.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PeerCredentials {
            pid: pid as u32,
            uid,
        })
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: zero is a valid initial representation of the kernel credential struct.
        let mut credentials: libc::ucred = unsafe { mem::zeroed() };
        let mut length = mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: credentials and length are writable outputs of the advertised sizes;
        // this reads kernel-generated credentials, never sender-authored bytes.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize != mem::size_of_val(&credentials) || credentials.pid <= 0 {
            return Err(io::Error::other("Kernel returned no peer credentials"));
        }
        Ok(PeerCredentials {
            pid: credentials.pid as u32,
            uid: credentials.uid,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix peer credentials are unsupported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
        process::Stdio,
        time::{Duration, Instant},
    };

    #[test]
    fn new_session_child_leads_its_own_group_and_group_signals_reach_it() {
        let mut command = Command::new("sleep");
        command.arg("30").stdin(Stdio::null());
        new_session(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        assert_eq!(session_id(child.id()).unwrap(), child.id());
        assert_ne!(session_id(std::process::id()).unwrap(), child.id());
        // SAFETY: getpgid only reads kernel state for a PID.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        terminate_process_group(child.id()).unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn session_id_of_a_reaped_or_impossible_process_is_an_error() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(session_id(pid).is_err());
        assert_eq!(
            session_id(u32::MAX).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn process_group_signals_reject_ids_that_would_address_other_processes() {
        assert_eq!(
            kill_process_group(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            terminate_process_group(u32::MAX).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn set_nonblocking_makes_an_empty_pipe_read_would_block() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        set_nonblocking(stdout.as_fd()).unwrap();
        let error = stdout.read(&mut [0; 8]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn unix_socket_connects_without_blocking_and_reports_this_user_as_peer() {
        let directory = std::env::temp_dir().join(format!("drostje-unix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("peer.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let mut stream = connect_unix_nonblocking(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let readiness = loop {
            if let Some(readiness) = poll_writable(stream.as_fd()).unwrap() {
                break readiness;
            }
            assert!(Instant::now() < deadline, "socket never became writable");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(readiness.writable && !readiness.hung_up);
        let (mut accepted, _) = listener.accept().unwrap();
        stream.write_all(b"ping").unwrap();
        let mut received = [0; 4];
        accepted.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"ping");
        let credentials = peer_credentials(&accepted).unwrap();
        assert_eq!(credentials.uid, effective_uid());
        assert_eq!(credentials.pid, std::process::id());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn readiness_wait_ends_on_wake_input_or_timeout() {
        let wake = WakePipe::new().unwrap();
        let started = Instant::now();
        wait_until_ready(&[wake.fd()], &[], Duration::from_millis(50)).unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "nothing was ready"
        );

        let waker = std::sync::Arc::new(wake);
        let signal = waker.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            signal.wake();
        });
        let started = Instant::now();
        wait_until_ready(&[waker.fd()], &[], Duration::from_secs(10)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wake ended the wait"
        );
        thread.join().unwrap();
        for _ in 0..100_000 {
            waker.wake();
        }
        waker.clear();
        let started = Instant::now();
        wait_until_ready(&[waker.fd()], &[], Duration::from_millis(20)).unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "clear consumed every wake"
        );

        let mut child = Command::new("sh")
            .args(["-c", "printf x"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let started = Instant::now();
        wait_until_ready(&[stdout.as_fd()], &[], Duration::from_secs(10)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child output ended the wait"
        );
        child.wait().unwrap();

        let (_reader, writer) = io::pipe().unwrap();
        let started = Instant::now();
        wait_until_ready(&[], &[writer.as_fd()], Duration::from_secs(10)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an empty pipe accepts writes"
        );
    }

    #[test]
    fn unix_socket_paths_with_nul_or_overflow_are_refused() {
        let long = Path::new("/").join("x".repeat(200));
        assert_eq!(
            connect_unix_nonblocking(&long).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            connect_unix_nonblocking(Path::new("/tmp/a\0b"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
