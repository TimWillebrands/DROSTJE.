#import <AppKit/AppKit.h>
#import <CoreVideo/CoreVideo.h>
#import <IOSurface/IOSurface.h>
#import <stdatomic.h>
#import <stdlib.h>
#import <string.h>
#import <ghostty.h>
#include "process_activity.h"
#include "native_input.h"

extern bool drost_ghostty_config_command(ghostty_config_t, const char *const *, size_t);
extern void drost_ghostty_config_theme(ghostty_config_t, const uint8_t *);
extern uint64_t drost_ghostty_surface_root_pid(ghostty_surface_t);
extern bool drost_ghostty_surface_login_wrapper(ghostty_surface_t);
extern const char *drost_ghostty_surface_expected_program(ghostty_surface_t);

@interface GpuiGhosttyView : NSView {
@public
    void *frame_userdata;
    void (*frame_ready)(void *);
}
@end

@implementation GpuiGhosttyView
- (NSView *)hitTest:(NSPoint)point {
    (void)point;
    return nil;
}
- (void)observeValueForKeyPath:(NSString *)keyPath ofObject:(id)object
                      change:(NSDictionary *)change context:(void *)context {
    (void)keyPath; (void)object; (void)change; (void)context;
    // Metal completion installs IOSurface contents asynchronously on the main
    // queue. Wake GPUI after that installation, not just before GPU submission.
    if (frame_ready != NULL) frame_ready(frame_userdata);
}
@end

typedef void (*gpui_ghostty_wakeup_cb)(void *userdata);
// Adapter operation values: paste=0, read=1, write=2.
typedef bool (*gpui_ghostty_approve_clipboard_cb)(void *userdata, int operation, const char *text);

typedef struct gpui_ghostty_surface {
    ghostty_config_t config;
    ghostty_app_t app;
    ghostty_surface_t surface;
    NSView *parent;
    GpuiGhosttyView *view;
    void *wakeup_userdata;
    gpui_ghostty_wakeup_cb wakeup;
    gpui_ghostty_approve_clipboard_cb approve_clipboard;
    _Atomic bool alive;
} gpui_ghostty_surface;

static void runtime_wakeup(void *userdata) {
    gpui_ghostty_surface *state = userdata;
    state->wakeup(state->wakeup_userdata);
}

static bool runtime_action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action) {
    (void)app;
    if (action.tag == GHOSTTY_ACTION_RENDER &&
        target.tag == GHOSTTY_TARGET_SURFACE &&
        target.target.surface != NULL) {
        ghostty_surface_draw(target.target.surface);
        return true;
    }
    return false;
}

static bool runtime_read_clipboard(void *userdata, ghostty_clipboard_e location, void *request) {
    (void)location;
    gpui_ghostty_surface *state = userdata;
    if (state->surface == NULL) return false;
    NSString *text = [[NSPasteboard generalPasteboard] stringForType:NSPasteboardTypeString];
    if (text == nil) return false;
    ghostty_surface_complete_clipboard_request(state->surface, text.UTF8String, request, false);
    return true;
}

static void runtime_confirm_read_clipboard(
    void *userdata,
    const char *text,
    void *request,
    ghostty_clipboard_request_e kind
) {
    gpui_ghostty_surface *state = userdata;
    if (state->surface != NULL) {
        int operation = kind == GHOSTTY_CLIPBOARD_REQUEST_PASTE ? 0 : 1;
        bool approved = state->approve_clipboard != NULL &&
            state->approve_clipboard(state->wakeup_userdata, operation, text);
        // Completing with empty text releases Ghostty's request without exposing
        // clipboard contents or inserting an unsafe paste.
        ghostty_surface_complete_clipboard_request(state->surface, approved ? text : "", request, true);
    }
}

static void runtime_write_clipboard(
    void *userdata,
    ghostty_clipboard_e location,
    const ghostty_clipboard_content_s *content,
    size_t count,
    bool confirm
) {
    gpui_ghostty_surface *state = userdata;
    (void)location;
    for (size_t index = 0; index < count; index++) {
        if (strcmp(content[index].mime, "text/plain") != 0) continue;
        if (confirm && (state->approve_clipboard == NULL ||
            !state->approve_clipboard(state->wakeup_userdata, 2, content[index].data))) return;
        NSString *text = [NSString stringWithUTF8String:content[index].data];
        if (text == nil) return;
        NSPasteboard *pasteboard = [NSPasteboard generalPasteboard];
        [pasteboard clearContents];
        [pasteboard setString:text forType:NSPasteboardTypeString];
        return;
    }
}

static void runtime_close_surface(void *userdata, bool process_alive) {
    (void)process_alive;
    gpui_ghostty_surface *state = userdata;
    atomic_store_explicit(&state->alive, false, memory_order_release);
    runtime_wakeup(state);
}

gpui_ghostty_surface *gpui_ghostty_surface_new(
    void *parent_view,
    const char *working_directory,
    const char *const *argv,
    size_t argc,
    const uint8_t *theme,
    void *wakeup_userdata,
    gpui_ghostty_wakeup_cb wakeup,
    gpui_ghostty_approve_clipboard_cb approve_clipboard
) {
    static dispatch_once_t once;
    static int init_result = -1;
    dispatch_once(&once, ^{
        setenv("GHOSTTY_LOG", "stderr", 0);
        init_result = ghostty_init(0, NULL);
    });
    if (init_result != GHOSTTY_SUCCESS || parent_view == NULL ||
        wakeup_userdata == NULL || wakeup == NULL) return NULL;

    gpui_ghostty_surface *state = calloc(1, sizeof(gpui_ghostty_surface));
    if (state == NULL) return NULL;
    atomic_init(&state->alive, true);
    state->wakeup_userdata = wakeup_userdata;
    state->wakeup = wakeup;
    state->approve_clipboard = approve_clipboard;
    state->parent = (NSView *)parent_view;
    state->view = [[GpuiGhosttyView alloc] initWithFrame:NSMakeRect(0, 0, 800, 600)];
    [state->view setHidden:YES];
    [state->parent addSubview:state->view];

    state->config = ghostty_config_new();
    if (state->config == NULL) goto fail;
    if (!drost_ghostty_config_command(state->config, argv, argc)) goto fail;
    if (theme != NULL) drost_ghostty_config_theme(state->config, theme);
    ghostty_config_finalize(state->config);

    ghostty_runtime_config_s runtime = {
        .userdata = state,
        .supports_selection_clipboard = false,
        .wakeup_cb = runtime_wakeup,
        .action_cb = runtime_action,
        .read_clipboard_cb = runtime_read_clipboard,
        .confirm_read_clipboard_cb = runtime_confirm_read_clipboard,
        .write_clipboard_cb = runtime_write_clipboard,
        .close_surface_cb = runtime_close_surface,
    };
    state->app = ghostty_app_new(&runtime, state->config);
    if (state->app == NULL) goto fail;

    ghostty_surface_config_s surface_config = ghostty_surface_config_new();
    surface_config.platform_tag = GHOSTTY_PLATFORM_MACOS;
    surface_config.platform.macos.nsview = state->view;
    surface_config.userdata = state;
    surface_config.scale_factor = state->parent.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor;
    ghostty_env_var_s environment[] = {
        { .key = "TERM", .value = "xterm-256color" },
        { .key = "COLORTERM", .value = "truecolor" },
        { .key = "TERM_PROGRAM", .value = "gpui-ghostty" },
    };
    surface_config.working_directory = working_directory;
    surface_config.env_vars = environment;
    surface_config.env_var_count = sizeof(environment) / sizeof(environment[0]);
    surface_config.wait_after_command = false;
    surface_config.context = GHOSTTY_SURFACE_CONTEXT_WINDOW;
    state->surface = ghostty_surface_new(state->app, &surface_config);
    if (state->surface == NULL) goto fail;
    state->view->frame_userdata = wakeup_userdata;
    state->view->frame_ready = wakeup;
    [state->view.layer addObserver:state->view forKeyPath:@"contents" options:0 context:NULL];

    ghostty_app_set_focus(state->app, false);
    ghostty_surface_set_focus(state->surface, false);
    return state;

fail:
    if (state->surface != NULL) ghostty_surface_free(state->surface);
    if (state->app != NULL) ghostty_app_free(state->app);
    if (state->config != NULL) ghostty_config_free(state->config);
    [state->view removeFromSuperview];
    [state->view release];
    free(state);
    return NULL;
}

void gpui_ghostty_surface_free(gpui_ghostty_surface *state) {
    if (state == NULL) return;
    state->view->frame_ready = NULL;
    [state->view.layer removeObserver:state->view forKeyPath:@"contents"];
    [state->view removeFromSuperview];
    if (state->surface != NULL) ghostty_surface_free(state->surface);
    if (state->app != NULL) ghostty_app_free(state->app);
    if (state->config != NULL) ghostty_config_free(state->config);
    [state->view release];
    free(state);
}

void gpui_ghostty_surface_tick(gpui_ghostty_surface *state) {
    if (state == NULL || state->app == NULL) return;
    ghostty_app_tick(state->app);
}

bool gpui_ghostty_surface_is_alive(const gpui_ghostty_surface *state) {
    return state != NULL && state->surface != NULL && !ghostty_surface_process_exited(state->surface);
}

bool gpui_ghostty_surface_process_info(
    const gpui_ghostty_surface *state, uint64_t *root, uint64_t *foreground,
    bool *login_wrapper
) {
    *root = 0;
    *foreground = 0;
    *login_wrapper = false;
    if (state == NULL || state->surface == NULL) return false;
    *root = drost_ghostty_surface_root_pid(state->surface);
    if (*root == 0) return false;
    *foreground = ghostty_surface_foreground_pid(state->surface);
    *login_wrapper = drost_ghostty_surface_login_wrapper(state->surface);
    return *foreground != 0;
}

const char *gpui_ghostty_surface_expected_program(const gpui_ghostty_surface *state) {
    if (state == NULL || state->surface == NULL) return NULL;
    return drost_ghostty_surface_expected_program(state->surface);
}

int gpui_ghostty_process_activity(
    uint32_t root, uint32_t foreground, bool login_wrapper, const char *expected_program
) {
    return drost_process_activity(root, foreground, login_wrapper, expected_program);
}

bool gpui_ghostty_surface_snapshot(
    gpui_ghostty_surface *state,
    uint8_t **pixels,
    uint32_t *width,
    uint32_t *height,
    size_t *length
) {
    if (pixels == NULL || width == NULL || height == NULL || length == NULL) return false;
    *pixels = NULL;
    *width = 0;
    *height = 0;
    *length = 0;
    if (state == NULL || state->view == nil) return false;

    id contents = state->view.layer.contents;
    // A successful capture with no pixels means the first GPU frame is pending.
    if (contents == nil) return true;
    if (CFGetTypeID((CFTypeRef)contents) != IOSurfaceGetTypeID()) return false;
    IOSurfaceRef surface = (IOSurfaceRef)contents;
    CFRetain(surface);
    if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) {
        CFRelease(surface);
        return false;
    }

    size_t surface_width = IOSurfaceGetWidth(surface);
    size_t surface_height = IOSurfaceGetHeight(surface);
    OSType pixel_format = IOSurfaceGetPixelFormat(surface);
    size_t source_stride = IOSurfaceGetBytesPerRow(surface);
    const uint8_t *source = IOSurfaceGetBaseAddress(surface);
    bool valid = pixel_format == kCVPixelFormatType_32BGRA && surface_width > 0 &&
        surface_height > 0 && source != NULL && surface_width <= UINT32_MAX &&
        surface_height <= UINT32_MAX &&
        surface_width <= SIZE_MAX / 4 && surface_height <= SIZE_MAX / (surface_width * 4) &&
        source_stride >= surface_width * 4;
    size_t destination_stride = valid ? surface_width * 4 : 0;
    size_t byte_length = valid ? destination_stride * surface_height : 0;
    uint8_t *copy = valid ? malloc(byte_length) : NULL;
    if (copy != NULL) {
        for (size_t row = 0; row < surface_height; row++) {
            memcpy(copy + row * destination_stride, source + row * source_stride, destination_stride);
        }
        *pixels = copy;
        *width = (uint32_t)surface_width;
        *height = (uint32_t)surface_height;
        *length = byte_length;
    }

    IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
    CFRelease(surface);
    return copy != NULL;
}

void gpui_ghostty_surface_snapshot_free(uint8_t *pixels) {
    free(pixels);
}

void gpui_ghostty_surface_set_frame(
    gpui_ghostty_surface *state,
    double x,
    double y,
    double width,
    double height
) {
    if (state == NULL || state->surface == NULL) return;
    double parent_height = NSHeight(state->parent.bounds);
    [state->view setFrame:NSMakeRect(x, parent_height - y - height, width, height)];
    double scale = state->parent.window.backingScaleFactor ?: NSScreen.mainScreen.backingScaleFactor;
    ghostty_surface_set_content_scale(state->surface, scale, scale);
    ghostty_surface_set_size(state->surface, (uint32_t)(width * scale), (uint32_t)(height * scale));
    ghostty_surface_refresh(state->surface);
}

void gpui_ghostty_surface_set_visible(gpui_ghostty_surface *state, bool visible) {
    if (state == NULL || state->surface == NULL) return;
    // Native child views never cover GPUI overlays; GPUI composes completed
    // Metal frame readbacks because its surface importer does not accept BGRA.
    [state->view setHidden:YES];
    ghostty_surface_set_occlusion(state->surface, visible);
    if (visible) ghostty_surface_refresh(state->surface);
}

void gpui_ghostty_surface_set_focus(gpui_ghostty_surface *state, bool focused) {
    if (state == NULL || state->surface == NULL) return;
    ghostty_app_set_focus(state->app, focused);
    ghostty_surface_set_focus(state->surface, focused);
}

bool gpui_ghostty_surface_key(
    gpui_ghostty_surface *state,
    int action,
    int modifiers,
    int consumed_modifiers,
    uint32_t keycode,
    const char *text,
    uint32_t unshifted_codepoint
) {
    if (state == NULL || state->surface == NULL) return false;
    ghostty_input_key_s event = {
        .action = (ghostty_input_action_e)action,
        .mods = (ghostty_input_mods_e)modifiers,
        .consumed_mods = (ghostty_input_mods_e)consumed_modifiers,
        .keycode = keycode,
        .text = text,
        .unshifted_codepoint = unshifted_codepoint,
        .composing = false,
    };
    return ghostty_surface_key(state->surface, event);
}

void gpui_ghostty_surface_text(gpui_ghostty_surface *state, const char *text, size_t length) {
    if (state != NULL) gpui_ghostty_commit_text(state->surface, text, length);
}

void gpui_ghostty_surface_preedit(gpui_ghostty_surface *state, const char *text, size_t length) {
    if (state != NULL && state->surface != NULL) ghostty_surface_preedit(state->surface, text, length);
}

void gpui_ghostty_surface_ime_point(gpui_ghostty_surface *state, double *x, double *y, double *w, double *h) {
    if (state != NULL && state->surface != NULL) ghostty_surface_ime_point(state->surface, x, y, w, h);
}

void gpui_ghostty_surface_theme(gpui_ghostty_surface *state, const uint8_t *theme) {
    if (state == NULL || state->surface == NULL) return;
    drost_ghostty_config_theme(state->config, theme);
    ghostty_config_finalize(state->config);
    ghostty_surface_update_config(state->surface, state->config);
    ghostty_surface_refresh(state->surface);
}


void gpui_ghostty_surface_mouse_position(
    gpui_ghostty_surface *state,
    double x,
    double y,
    int modifiers
) {
    if (state != NULL && state->surface != NULL) {
        ghostty_surface_mouse_pos(state->surface, x, y, (ghostty_input_mods_e)modifiers);
    }
}

void gpui_ghostty_surface_mouse_button(
    gpui_ghostty_surface *state,
    int mouse_state,
    int button,
    int modifiers
) {
    if (state != NULL && state->surface != NULL) {
        ghostty_surface_mouse_button(
            state->surface,
            (ghostty_input_mouse_state_e)mouse_state,
            (ghostty_input_mouse_button_e)button,
            (ghostty_input_mods_e)modifiers
        );
    }
}

void gpui_ghostty_surface_mouse_scroll(
    gpui_ghostty_surface *state,
    double x,
    double y,
    int modifiers
) {
    if (state != NULL && state->surface != NULL) {
        ghostty_surface_mouse_scroll(state->surface, x, y, modifiers);
    }
}
