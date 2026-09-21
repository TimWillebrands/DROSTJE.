const std = @import("std");
const builtin = @import("builtin");

// Default shells are executable paths, not shell source. Explicit commands
// bypass this helper so their arguments and startup semantics stay unchanged.
pub fn argv(alloc: std.mem.Allocator, shell: [:0]const u8) ![]const [:0]const u8 {
    // macOS login(1) uses -l to preserve cwd, which disables its automatic
    // login-shell argv[0]. Ask the shell itself to load login profiles.
    const login = builtin.os.tag.isDarwin();
    const args = try alloc.alloc([:0]const u8, if (login) 2 else 1);
    args[0] = shell;
    if (login) args[1] = "-l";
    return args;
}

test "macOS default shell loads login profile before interactive configuration" {
    if (builtin.os.tag != .macos) return error.SkipZigTest;
    const testing = std.testing;
    const alloc = testing.allocator;
    var tmp = testing.tmpDir(.{});
    defer tmp.cleanup();
    const path = try tmp.dir.realPathFileAlloc(testing.io, ".", alloc);
    defer alloc.free(path);
    try tmp.dir.writeFile(testing.io, .{
        .sub_path = ".zprofile",
        .data = "export DROSTJE_PROFILE=profile-loaded\n",
    });
    try tmp.dir.writeFile(testing.io, .{
        .sub_path = ".zshrc",
        .data = "export DROSTJE_INTERACTIVE=${DROSTJE_PROFILE:-profile-missing}\n",
    });
    var env = std.process.Environ.Map.init(alloc);
    defer env.deinit();
    try env.put("HOME", path);
    try env.put("ZDOTDIR", path);
    try env.put("PATH", "/usr/bin:/bin");
    const shell_args = try argv(alloc, "/bin/zsh");
    defer alloc.free(shell_args);
    var args: std.ArrayList([]const u8) = .empty;
    defer args.deinit(alloc);
    for (shell_args) |arg| try args.append(alloc, arg);
    try args.appendSlice(alloc, &.{ "-i", "-c", "printf '%s' \"$DROSTJE_INTERACTIVE\"" });
    const result = try std.process.run(alloc, testing.io, .{
        .argv = args.items,
        .environ_map = &env,
    });
    defer alloc.free(result.stdout);
    defer alloc.free(result.stderr);
    try testing.expectEqual(std.process.Child.Term{ .exited = 0 }, result.term);
    try testing.expectEqualStrings("profile-loaded", result.stdout);
}
