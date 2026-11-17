use std::sync::Mutex;

static LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<async_channel::Sender<()>>> = Mutex::new(None);

pub fn signal() -> async_channel::Receiver<()> {
    let (sender, receiver) = async_channel::bounded(1);
    *WAKE.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(sender);
    receiver
}

pub fn take() -> Vec<String> {
    std::mem::take(&mut *LINKS.lock().unwrap_or_else(|poison| poison.into_inner()))
}

pub fn deliver(link: String) {
    LINKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(link);
    if let Some(wake) = WAKE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
    {
        let _ = wake.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_wait_for_the_newest_listener_and_arrive_once() {
        let stale = signal();
        let current = signal();
        deliver("drostje://pair?v=1".into());
        assert!(stale.try_recv().is_err());
        assert!(current.try_recv().is_ok());
        assert_eq!(take(), vec!["drostje://pair?v=1".to_owned()]);
        assert!(take().is_empty());
    }
}
