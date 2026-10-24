#define _GNU_SOURCE
#include "wayland_client.h"
#include "input-method-client-protocol.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <xkbcommon/xkbcommon-keysyms.h>
#include <xkbcommon/xkbcommon-names.h>

#define MAX_TEXT (1024u * 1024u)
#define MAX_COMMIT 4079u
#define MAX_IMAGE (64u * 1024u * 1024u)
#define MAX_KEY 768u

struct image_buffer {
    struct wl_buffer *buffer;
    void *data;
    size_t size;
    int width, height, stride;
    bool busy, erase;
    struct inklow_wl *owner;
};

struct inklow_wl {
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct wl_seat *seat;
    struct wl_pointer *pointer;
    struct zwp_input_method_v1 *method;
    struct zwp_input_method_context_v1 *context;
    struct zwp_input_panel_v1 *panel;
    struct zwp_input_panel_surface_v1 *panel_surface;
    struct wl_surface *surface;
    struct wl_keyboard *keyboard;
    struct xkb_context *xkb;
    struct xkb_keymap *keymap;
    struct xkb_state *keys;
    struct image_buffer buffers[2];
    inklow_wl_callback callback;
    void *userdata;
    char *text;
    size_t length, cursor, text_capacity;
    uint32_t generation, serial, modifiers;
    uint64_t context_id;
    uint32_t method_id, panel_id, compositor_id, shm_id, seat_id;
    bool known, safe, surrounding, ready, failed, visible, pointer_inside, initial_state_pending;
    bool consumed[MAX_KEY];
    int32_t pointer_x, pointer_y;
};

static void wipe(void *data, size_t length)
{
    volatile unsigned char *p = data;
    while (length--) *p++ = 0;
}

static void clear_text(struct inklow_wl *w)
{
    if (w->text) {
        wipe(w->text, w->length);
        w->text[0] = '\0';
    }
    w->length = w->cursor = 0;
    w->surrounding = false;
}

static void next_generation(struct inklow_wl *w)
{
    if (++w->generation == 0) ++w->generation;
}

static int emit(struct inklow_wl *w, struct inklow_wl_event *event)
{
    event->generation = w->generation;
    return w->callback ? w->callback(w->userdata, event) : 0;
}

static void state_event(struct inklow_wl *w)
{
    struct inklow_wl_event event = {
        .type = 1, .serial = w->serial,
        .state = w->context && w->initial_state_pending && !w->failed
            ? (!w->known ? 1u : (w->safe ? 2u : 0u)) : 0u
    };
    emit(w, &event);
}

void inklow_wl_hide(struct inklow_wl *w)
{
    if (!w) return;
    if (w->surface && w->visible) {
        wl_surface_attach(w->surface, NULL, 0, 0);
        wl_surface_commit(w->surface);
    }
    w->visible = w->pointer_inside = false;
    for (size_t i = 0; i < 2; ++i) {
        struct image_buffer *b = &w->buffers[i];
        b->erase = true;
        if (!b->busy && b->data) {
            wipe(b->data, b->size);
            b->erase = false;
        }
    }
    if (w->display && !w->failed) inklow_wl_flush(w);
}

static void invalidate(struct inklow_wl *w, bool erase_text)
{
    w->ready = false;
    next_generation(w);
    if (erase_text) clear_text(w);
    inklow_wl_hide(w);
}

static void drop_keyboard(struct inklow_wl *w)
{
    if (w->keyboard) {
        /* KWin creates input-method wl_keyboard resources at version 10 even
         * though the v1 constructor's client proxy inherits version 1. KWin
         * implements release; a local-only destroy would leave its grab alive.
         */
        wl_keyboard_release(w->keyboard);
        w->keyboard = NULL;
    }
    if (w->keys) xkb_state_unref(w->keys);
    if (w->keymap) xkb_keymap_unref(w->keymap);
    w->keys = NULL;
    w->keymap = NULL;
    w->modifiers = 128;
    memset(w->consumed, 0, sizeof(w->consumed));
}

static void connection_error(struct inklow_wl *w)
{
    if (w->failed) return;
    w->failed = true;
    w->known = w->safe = false;
    invalidate(w, true);
    drop_keyboard(w);
    if (w->context) zwp_input_method_context_v1_destroy(w->context);
    w->context = NULL;
    /* Global removal can leave the transport alive. Send the unmap and grab
     * release without re-entering the failure-reporting flush wrapper.
     */
    if (w->display) wl_display_flush(w->display);
    struct inklow_wl_event event = {.type = 4, .name = "Wayland input-method connection unavailable"};
    emit(w, &event);
}

int inklow_wl_flush(struct inklow_wl *w)
{
    if (!w || !w->display || w->failed) return -1;
    if (wl_display_flush(w->display) >= 0) return 1;
    if (errno == EAGAIN) return 0;
    connection_error(w);
    return -1;
}

static bool utf8(const char *text, size_t length)
{
    const unsigned char *s = (const unsigned char *)text;
    for (size_t i = 0; i < length;) {
        uint32_t c = s[i++];
        if (c == 0) return false;
        if (c < 0x80) continue;
        unsigned n;
        uint32_t minimum;
        if (c >= 0xc2 && c <= 0xdf) { n = 1; minimum = 0x80; c &= 0x1f; }
        else if (c >= 0xe0 && c <= 0xef) { n = 2; minimum = 0x800; c &= 0x0f; }
        else if (c >= 0xf0 && c <= 0xf4) { n = 3; minimum = 0x10000; c &= 7; }
        else return false;
        if (n > length - i) return false;
        while (n--) {
            if ((s[i] & 0xc0) != 0x80) return false;
            c = (c << 6) | (s[i++] & 0x3f);
        }
        if (c < minimum || c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff)) return false;
    }
    return true;
}

static bool boundary(const char *text, size_t length, size_t offset)
{
    return offset <= length && (offset == length || ((unsigned char)text[offset] & 0xc0) != 0x80);
}

static uint32_t canonical_modifiers(struct inklow_wl *w)
{
    if (!w->keys || !w->keymap) return 128;
    uint32_t result = 0;
    xkb_mod_mask_t canonical = 0;
    const char *names[] = {XKB_MOD_NAME_SHIFT, XKB_MOD_NAME_CTRL, XKB_MOD_NAME_ALT, XKB_MOD_NAME_LOGO};
    const uint32_t bits[] = {1, 4, 8, 64};
    for (size_t i = 0; i < 4; ++i) {
        xkb_mod_index_t index = xkb_keymap_mod_get_index(w->keymap, names[i]);
        if (index != XKB_MOD_INVALID && index < sizeof(xkb_mod_mask_t) * CHAR_BIT) {
            canonical |= (xkb_mod_mask_t)1 << index;
            if (xkb_state_mod_index_is_active(w->keys, index, XKB_STATE_MODS_EFFECTIVE) > 0) result |= bits[i];
        }
    }
    /* Virtual aliases (Alt, Meta, Super, Hyper, LevelThree, etc.) can share
     * real masks. Ignore aliases of canonical real modifiers, but explicitly
     * retain level switching as unsupported even when mapped onto Mod1/Mod4.
     */
    const char *levels[] = {"LevelThree", "LevelFive", "AltGr"};
    for (size_t i = 0; i < 3; ++i)
        if (xkb_state_mod_name_is_active(w->keys, levels[i], XKB_STATE_MODS_EFFECTIVE) > 0) result |= 128;
    xkb_mod_mask_t active = xkb_state_serialize_mods(w->keys, XKB_STATE_MODS_EFFECTIVE);
    const char *locks[] = {XKB_MOD_NAME_CAPS, XKB_MOD_NAME_NUM};
    for (size_t i = 0; i < 2; ++i) {
        xkb_mod_index_t index = xkb_keymap_mod_get_index(w->keymap, locks[i]);
        if (index != XKB_MOD_INVALID && index < sizeof(xkb_mod_mask_t) * CHAR_BIT)
            canonical |= (xkb_mod_mask_t)1 << index;
    }
    /* serialize_mods includes real modifier bits; virtual modifier aliases
     * must not turn an ordinary Ctrl/Alt/Super chord into an unknown chord.
     * The eight standard real modifier slots are defined by XKB keymaps.
     */
    if ((active & 0xffu) & ~canonical) result |= 128;
    for (xkb_mod_index_t i = 8; i < xkb_keymap_num_mods(w->keymap); ++i) {
        if (xkb_state_mod_index_is_active(w->keys, i, XKB_STATE_MODS_EFFECTIVE) <= 0) continue;
        const char *name = xkb_keymap_mod_get_name(w->keymap, i);
        if (!name || (strcmp(name, "Alt") && strcmp(name, "Meta") && strcmp(name, "Super") &&
                      strcmp(name, "NumLock") && strcmp(name, "Control") && strcmp(name, "Shift")))
            result |= 128;
    }
    return result;
}

static void keyboard_keymap(void *data, struct wl_keyboard *keyboard, uint32_t format, int32_t fd, uint32_t size)
{
    (void)keyboard;
    struct inklow_wl *w = data;
    struct xkb_keymap *map = NULL;
    struct xkb_state *state = NULL;
    struct stat info;
    if (format == WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 && size > 0 && size <= MAX_TEXT &&
        fstat(fd, &info) == 0 && info.st_size >= (off_t)size) {
        char *text = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
        if (text != MAP_FAILED) {
            if (text[size - 1] == '\0')
                map = xkb_keymap_new_from_string(w->xkb, text, XKB_KEYMAP_FORMAT_TEXT_V1, XKB_KEYMAP_COMPILE_NO_FLAGS);
            munmap(text, size);
        }
    }
    close(fd);
    if (map) state = xkb_state_new(map);
    if (w->keys) xkb_state_unref(w->keys);
    if (w->keymap) xkb_keymap_unref(w->keymap);
    w->keymap = map;
    w->keys = state;
    w->modifiers = canonical_modifiers(w);
}

static void keyboard_enter(void *data, struct wl_keyboard *keyboard, uint32_t serial,
                           struct wl_surface *surface, struct wl_array *keys)
{
    (void)keyboard; (void)serial; (void)surface; (void)keys;
    struct inklow_wl *w = data;
    memset(w->consumed, 0, sizeof(w->consumed));
}

static void keyboard_leave(void *data, struct wl_keyboard *keyboard, uint32_t serial, struct wl_surface *surface)
{
    (void)keyboard; (void)serial; (void)surface;
    struct inklow_wl *w = data;
    invalidate(w, true);
    memset(w->consumed, 0, sizeof(w->consumed));
    state_event(w);
}

static bool modifier_keysym(xkb_keysym_t sym)
{
    return (sym >= XKB_KEY_Shift_L && sym <= XKB_KEY_Hyper_R) ||
           sym == XKB_KEY_ISO_Level3_Shift || sym == XKB_KEY_ISO_Level5_Shift ||
           sym == XKB_KEY_Mode_switch || sym == XKB_KEY_Num_Lock;
}

static void keyboard_key(void *data, struct wl_keyboard *keyboard, uint32_t serial,
                         uint32_t time, uint32_t key, uint32_t state)
{
    (void)keyboard;
    struct inklow_wl *w = data;
    struct zwp_input_method_context_v1 *context = w->context;
    if (!context || w->failed) return;
    bool consume = key < MAX_KEY && w->consumed[key];
    xkb_keysym_t sym = w->keys && key < MAX_KEY
        ? xkb_state_key_get_one_sym(w->keys, key + 8) : XKB_KEY_NoSymbol;
    if (state == WL_KEYBOARD_KEY_STATE_RELEASED && key < MAX_KEY)
        w->consumed[key] = false;
    if (w->ready && w->keys && key < MAX_KEY &&
        (state == WL_KEYBOARD_KEY_STATE_RELEASED || (!consume && state == WL_KEYBOARD_KEY_STATE_PRESSED))) {
        char name[128];
        const xkb_keysym_t *base = NULL;
        xkb_layout_index_t layout = xkb_state_key_get_layout(w->keys, key + 8);
        if (layout != XKB_LAYOUT_INVALID &&
            xkb_keymap_key_get_syms_by_level(w->keymap, key + 8, layout, 0, &base) == 1 &&
            ((base[0] >= XKB_KEY_0 && base[0] <= XKB_KEY_9) ||
             (base[0] >= XKB_KEY_A && base[0] <= XKB_KEY_Z) ||
             (base[0] >= XKB_KEY_a && base[0] <= XKB_KEY_z))) {
            sym = base[0];
        }
        if (xkb_keysym_get_name(sym, name, sizeof(name)) > 0) {
            struct inklow_wl_event event = {.type = 2, .serial = serial, .time = time,
                .key = key + 8, .state = state, .modifiers = w->modifiers, .name = name};
            int handled = emit(w, &event);
            /* A release callback cannot consume a key whose press was passed. */
            if (state == WL_KEYBOARD_KEY_STATE_PRESSED && handled) {
                consume = true;
                w->consumed[key] = true;
            }
        }
    }
    if (!consume && w->context == context && !w->failed) {
        zwp_input_method_context_v1_key(context, serial, time, key, state);
        if (state != WL_KEYBOARD_KEY_STATE_RELEASED && !modifier_keysym(sym)) {
            invalidate(w, true);
            state_event(w);
        }
    }
    inklow_wl_flush(w);
}

static void keyboard_modifiers(void *data, struct wl_keyboard *keyboard, uint32_t serial,
                               uint32_t depressed, uint32_t latched, uint32_t locked, uint32_t group)
{
    (void)keyboard;
    struct inklow_wl *w = data;
    if (w->keys) xkb_state_update_mask(w->keys, depressed, latched, locked, 0, 0, group);
    w->modifiers = canonical_modifiers(w);
    if (w->context && !w->failed)
        zwp_input_method_context_v1_modifiers(w->context, serial, depressed, latched, locked, group);
}

static void keyboard_repeat(void *data, struct wl_keyboard *keyboard, int32_t rate, int32_t delay)
{
    /* Forward hardware events verbatim; do not synthesize shortcut repeats. */
    (void)data; (void)keyboard; (void)rate; (void)delay;
}

static const struct wl_keyboard_listener keyboard_listener = {
    .keymap = keyboard_keymap, .enter = keyboard_enter, .leave = keyboard_leave,
    .key = keyboard_key, .modifiers = keyboard_modifiers, .repeat_info = keyboard_repeat,
};

static void ensure_keyboard(struct inklow_wl *w)
{
    if (!w->keyboard && w->context && w->safe && !w->failed) {
        w->keyboard = zwp_input_method_context_v1_grab_keyboard(w->context);
        if (w->keyboard) wl_keyboard_add_listener(w->keyboard, &keyboard_listener, w);
    }
}

static void context_surrounding(void *data, struct zwp_input_method_context_v1 *context,
                                const char *text, uint32_t cursor, uint32_t anchor)
{
    struct inklow_wl *w = data;
    if (context != w->context) return;
    invalidate(w, true);
    if (w->known && w->safe && text) {
        size_t length = strnlen(text, MAX_TEXT + 1);
        if (length <= MAX_TEXT && cursor == anchor && boundary(text, length, cursor) && utf8(text, length)) {
            if (length + 1 > w->text_capacity) {
                size_t capacity = w->text_capacity ? w->text_capacity : 256;
                while (capacity < length + 1 && capacity < MAX_TEXT + 1) {
                    capacity = capacity > (MAX_TEXT + 1) / 2 ? MAX_TEXT + 1 : capacity * 2;
                }
                char *text_buffer = realloc(w->text, capacity);
                if (text_buffer) {
                    w->text = text_buffer;
                    w->text_capacity = capacity;
                }
            }
            if (length + 1 <= w->text_capacity) {
                memcpy(w->text, text, length + 1);
                w->length = length;
                w->cursor = cursor;
                w->surrounding = true;
            }
        }
    }
    state_event(w);
}

static void context_reset(void *data, struct zwp_input_method_context_v1 *context)
{
    struct inklow_wl *w = data;
    if (context != w->context) return;
    invalidate(w, true);
    state_event(w);
}

static void context_content(void *data, struct zwp_input_method_context_v1 *context, uint32_t hint, uint32_t purpose)
{
    struct inklow_wl *w = data;
    if (context != w->context) return;
    /* v1 purposes 0..12 are defined; 8 is password. KWin translates v3 PIN
     * into password. Unknown hints/purposes fail closed for future extensions.
     */
    bool safe = !(hint & (0xc0u | ~0x3ffu)) && purpose <= 12 && purpose != 8;
    if (!w->known || w->safe != safe) invalidate(w, true);
    else invalidate(w, false);
    w->known = true;
    w->safe = safe;
    if (safe) ensure_keyboard(w);
    /* Keep an existing grab strictly pass-through while privacy-disabled.
     * Releasing it here drops in-flight keys or modifiers during the handoff.
     * No key callback runs until safe committed surrounding text returns.
     */
    state_event(w);
}

static void context_action(void *data, struct zwp_input_method_context_v1 *context, uint32_t button, uint32_t index)
{
    (void)data; (void)context; (void)button; (void)index;
    /* No preedit is installed, so there is no composing span to invoke. */
}

static void context_commit(void *data, struct zwp_input_method_context_v1 *context, uint32_t serial)
{
    struct inklow_wl *w = data;
    if (context != w->context) return;
    /* Even an otherwise unchanged committed state supersedes a rendered
     * anchor. Reused application serials must not revive an old snapshot.
     */
    if (w->ready) invalidate(w, false);
    w->serial = serial;
    w->ready = w->known && w->safe && w->surrounding && !w->failed;
    if (w->ready) w->initial_state_pending = false;
    if (!w->ready) inklow_wl_hide(w);
    state_event(w);
}

static void context_language(void *data, struct zwp_input_method_context_v1 *context, const char *language)
{
    (void)data; (void)context; (void)language;
    /* Prediction language is an explicit user setting, not field metadata. */
}

static const struct zwp_input_method_context_v1_listener context_listener = {
    .surrounding_text = context_surrounding, .reset = context_reset,
    .content_type = context_content, .invoke_action = context_action,
    .commit_state = context_commit, .preferred_language = context_language,
};

static void release_context(struct inklow_wl *w)
{
    invalidate(w, true);
    drop_keyboard(w);
    if (w->context) zwp_input_method_context_v1_destroy(w->context);
    w->context = NULL;
    w->known = w->safe = false;
    w->serial = 0;
}

static void method_activate(void *data, struct zwp_input_method_v1 *method, struct zwp_input_method_context_v1 *context)
{
    (void)method;
    struct inklow_wl *w = data;
    release_context(w);
    w->context = context;
    w->initial_state_pending = true;
    if (++w->context_id == 0) ++w->context_id;
    zwp_input_method_context_v1_add_listener(context, &context_listener, w);
    state_event(w);
}

static void method_deactivate(void *data, struct zwp_input_method_v1 *method, struct zwp_input_method_context_v1 *context)
{
    (void)method;
    struct inklow_wl *w = data;
    if (context == w->context) {
        release_context(w);
        state_event(w);
    }
}

static const struct zwp_input_method_v1_listener method_listener = {
    .activate = method_activate, .deactivate = method_deactivate,
};

static void pointer_enter(void *data, struct wl_pointer *pointer, uint32_t serial,
                          struct wl_surface *surface, wl_fixed_t x, wl_fixed_t y)
{
    (void)pointer; (void)serial;
    struct inklow_wl *w = data;
    w->pointer_inside = surface == w->surface && w->visible && w->ready;
    w->pointer_x = wl_fixed_to_int(x);
    w->pointer_y = wl_fixed_to_int(y);
}

static void pointer_leave(void *data, struct wl_pointer *pointer, uint32_t serial, struct wl_surface *surface)
{
    (void)pointer; (void)serial; (void)surface;
    ((struct inklow_wl *)data)->pointer_inside = false;
}

static void pointer_motion(void *data, struct wl_pointer *pointer, uint32_t time, wl_fixed_t x, wl_fixed_t y)
{
    (void)pointer; (void)time;
    struct inklow_wl *w = data;
    w->pointer_x = wl_fixed_to_int(x);
    w->pointer_y = wl_fixed_to_int(y);
}

static void pointer_button(void *data, struct wl_pointer *pointer, uint32_t serial,
                           uint32_t time, uint32_t button, uint32_t state)
{
    (void)pointer; (void)serial;
    struct inklow_wl *w = data;
    if (!w->pointer_inside || !w->visible || !w->ready || w->failed) return;
    struct inklow_wl_event event = {.type = 3, .serial = w->serial, .time = time,
        .key = button, .state = state, .x = w->pointer_x, .y = w->pointer_y};
    emit(w, &event);
}

static void pointer_axis(void *data, struct wl_pointer *pointer, uint32_t time, uint32_t axis, wl_fixed_t value)
{
    (void)data; (void)pointer; (void)time; (void)axis; (void)value;
}
static void pointer_frame(void *data, struct wl_pointer *pointer) { (void)data; (void)pointer; }
static void pointer_source(void *data, struct wl_pointer *pointer, uint32_t source) { (void)data; (void)pointer; (void)source; }
static void pointer_stop(void *data, struct wl_pointer *pointer, uint32_t time, uint32_t axis) { (void)data; (void)pointer; (void)time; (void)axis; }
static void pointer_discrete(void *data, struct wl_pointer *pointer, uint32_t axis, int32_t discrete) { (void)data; (void)pointer; (void)axis; (void)discrete; }
static const struct wl_pointer_listener pointer_listener = {
    .enter = pointer_enter, .leave = pointer_leave, .motion = pointer_motion,
    .button = pointer_button, .axis = pointer_axis, .frame = pointer_frame,
    .axis_source = pointer_source, .axis_stop = pointer_stop, .axis_discrete = pointer_discrete,
};

static void seat_capabilities(void *data, struct wl_seat *seat, uint32_t capabilities)
{
    struct inklow_wl *w = data;
    if ((capabilities & WL_SEAT_CAPABILITY_POINTER) && !w->pointer) {
        w->pointer = wl_seat_get_pointer(seat);
        if (w->pointer) wl_pointer_add_listener(w->pointer, &pointer_listener, w);
    } else if (!(capabilities & WL_SEAT_CAPABILITY_POINTER) && w->pointer) {
        if (wl_pointer_get_version(w->pointer) >= WL_POINTER_RELEASE_SINCE_VERSION) wl_pointer_release(w->pointer);
        else wl_pointer_destroy(w->pointer);
        w->pointer = NULL;
        w->pointer_inside = false;
    }
}
static void seat_name(void *data, struct wl_seat *seat, const char *name) { (void)data; (void)seat; (void)name; }
static const struct wl_seat_listener seat_listener = {.capabilities = seat_capabilities, .name = seat_name};

static void registry_global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version)
{
    struct inklow_wl *w = data;
    if (!strcmp(interface, "zwp_input_method_v1") && !w->method) {
        w->method = wl_registry_bind(registry, name, &zwp_input_method_v1_interface, 1);
        w->method_id = name;
        if (w->method) zwp_input_method_v1_add_listener(w->method, &method_listener, w);
    } else if (!strcmp(interface, "zwp_input_panel_v1") && !w->panel) {
        w->panel = wl_registry_bind(registry, name, &zwp_input_panel_v1_interface, 1);
        w->panel_id = name;
    } else if (!strcmp(interface, "wl_compositor") && !w->compositor) {
        w->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, version < 4 ? version : 4);
        w->compositor_id = name;
    } else if (!strcmp(interface, "wl_shm") && !w->shm) {
        w->shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
        w->shm_id = name;
    } else if (!strcmp(interface, "wl_seat") && !w->seat) {
        w->seat = wl_registry_bind(registry, name, &wl_seat_interface, version < 5 ? version : 5);
        w->seat_id = name;
        wl_seat_add_listener(w->seat, &seat_listener, w);
    }
}
static void registry_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    (void)registry;
    struct inklow_wl *w = data;
    if (name == w->method_id || name == w->panel_id || name == w->compositor_id || name == w->shm_id || name == w->seat_id)
        connection_error(w);
}
static const struct wl_registry_listener registry_listener = {.global = registry_global, .global_remove = registry_remove};

struct inklow_wl *inklow_wl_connect(int owned_fd, inklow_wl_callback callback, void *userdata)
{
    if (owned_fd < 0) return NULL;
    int type;
    socklen_t type_length = sizeof(type);
    struct sockaddr_storage address;
    socklen_t address_length = sizeof(address);
    if (getsockopt(owned_fd, SOL_SOCKET, SO_TYPE, &type, &type_length) || type != SOCK_STREAM ||
        getpeername(owned_fd, (struct sockaddr *)&address, &address_length) || address.ss_family != AF_UNIX ||
        fcntl(owned_fd, F_SETFD, FD_CLOEXEC) < 0) {
        close(owned_fd);
        return NULL;
    }
    struct inklow_wl *w = calloc(1, sizeof(*w));
    if (!w) { close(owned_fd); return NULL; }
    w->callback = callback;
    w->userdata = userdata;
    w->generation = 1;
    w->modifiers = 128;
    /* connect_to_fd consumes fd, including allocation failure; never consult
     * WAYLAND_DISPLAY and never fall back to a public display connection.
     */
    w->display = wl_display_connect_to_fd(owned_fd);
    if (!w->display) { free(w); return NULL; }
    w->xkb = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!w->xkb) { inklow_wl_close(w); return NULL; }
    w->registry = wl_display_get_registry(w->display);
    if (!w->registry) { inklow_wl_close(w); return NULL; }
    wl_registry_add_listener(w->registry, &registry_listener, w);
    if (wl_display_roundtrip(w->display) < 0 || !w->method || !w->panel || !w->compositor || !w->shm ||
        wl_display_roundtrip(w->display) < 0 || w->failed) {
        inklow_wl_close(w);
        return NULL;
    }
    w->surface = wl_compositor_create_surface(w->compositor);
    if (!w->surface) { inklow_wl_close(w); return NULL; }
    w->panel_surface = zwp_input_panel_v1_get_input_panel_surface(w->panel, w->surface);
    if (!w->panel_surface) { inklow_wl_close(w); return NULL; }
    zwp_input_panel_surface_v1_set_overlay_panel(w->panel_surface);
    if (inklow_wl_flush(w) < 0) { inklow_wl_close(w); return NULL; }
    return w;
}

int inklow_wl_fd(struct inklow_wl *w)
{
    return w && w->display && !w->failed ? wl_display_get_fd(w->display) : -1;
}

int inklow_wl_dispatch(struct inklow_wl *w)
{
    if (!w || !w->display || w->failed) return -1;
    while (wl_display_prepare_read(w->display) != 0) {
        if (wl_display_dispatch_pending(w->display) < 0 || w->failed) {
            connection_error(w);
            return -1;
        }
    }
    struct pollfd fd = {.fd = wl_display_get_fd(w->display), .events = POLLIN};
    int result;
    do { result = poll(&fd, 1, 0); } while (result < 0 && errno == EINTR);
    if (result < 0 || (fd.revents & (POLLERR | POLLHUP | POLLNVAL))) {
        wl_display_cancel_read(w->display);
        connection_error(w);
        return -1;
    }
    if (fd.revents & POLLIN) {
        if (wl_display_read_events(w->display) < 0) { connection_error(w); return -1; }
    } else wl_display_cancel_read(w->display);
    if (wl_display_dispatch_pending(w->display) < 0 || w->failed) { connection_error(w); return -1; }
    return inklow_wl_flush(w);
}

int inklow_wl_state(struct inklow_wl *w, uint32_t *generation, uint32_t *serial)
{
    if (!w || !w->ready || !w->context || w->failed) return 0;
    if (generation) *generation = w->generation;
    if (serial) *serial = w->serial;
    return 1;
}

uint64_t inklow_wl_context(struct inklow_wl *w)
{
    return w && w->context && !w->failed ? w->context_id : 0;
}

int inklow_wl_matches(struct inklow_wl *w, uint32_t generation, uint32_t serial,
                      const char *text, size_t length, size_t cursor)
{
    if (!inklow_wl_state(w, NULL, NULL) || generation != w->generation || serial != w->serial ||
        !text || length > MAX_TEXT || !boundary(text, length, cursor) || !utf8(text, length)) return 0;
    /* An empty native excerpt cannot corroborate a nonempty field. */
    if (!w->length && length) return 0;
    size_t suffix = w->length - w->cursor;
    if (w->cursor > cursor || suffix > length - cursor) return 0;
    return !memcmp(w->text, text + cursor - w->cursor, w->cursor) &&
           !memcmp(w->text + w->cursor, text + cursor, suffix);
}

int inklow_wl_commit(struct inklow_wl *w, uint32_t generation, uint32_t serial, const char *text)
{
    if (!inklow_wl_state(w, NULL, NULL) || generation != w->generation || serial != w->serial || !text) return 0;
    size_t length = strnlen(text, MAX_COMMIT + 1);
    if (length > MAX_COMMIT || !utf8(text, length)) return 0;
    zwp_input_method_context_v1_commit_string(w->context, serial, text);
    invalidate(w, true);
    state_event(w);
    return inklow_wl_flush(w) >= 0;
}

static void buffer_release(void *data, struct wl_buffer *buffer)
{
    (void)buffer;
    struct image_buffer *b = data;
    b->busy = false;
    if (b->erase && b->data) { wipe(b->data, b->size); b->erase = false; }
    if (b->owner && b->owner->ready && !b->owner->failed) {
        struct inklow_wl_event event = {.type = 5, .serial = b->owner->serial};
        emit(b->owner, &event);
    }
}
static const struct wl_buffer_listener buffer_listener = {.release = buffer_release};

static void destroy_buffer(struct image_buffer *b)
{
    if (b->buffer) wl_buffer_destroy(b->buffer);
    if (b->data) {
        /* A busy mapping may still be read by the compositor. Unmap our copy
         * without modifying it; detaching the surface removes it from UI.
         */
        if (!b->busy) wipe(b->data, b->size);
        munmap(b->data, b->size);
    }
    memset(b, 0, sizeof(*b));
}

static bool allocate_buffer(struct inklow_wl *w, struct image_buffer *b, int width, int height, int stride)
{
    size_t size = (size_t)stride * (size_t)height;
    int fd = memfd_create("inklow-input-panel", MFD_CLOEXEC);
    if (fd < 0) return false;
    if (ftruncate(fd, (off_t)size)) { close(fd); return false; }
    void *data = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (data == MAP_FAILED) { close(fd); return false; }
    struct wl_shm_pool *pool = wl_shm_create_pool(w->shm, fd, (int32_t)size);
    close(fd);
    if (!pool) { munmap(data, size); return false; }
    struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, width, height, stride, WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);
    if (!buffer) { munmap(data, size); return false; }
    b->buffer = buffer;
    b->data = data;
    b->size = size;
    b->width = width;
    b->height = height;
    b->stride = stride;
    b->owner = w;
    wl_buffer_add_listener(buffer, &buffer_listener, b);
    return true;
}

int inklow_wl_present(struct inklow_wl *w, const void *pixels, int width, int height, int stride, int scale)
{
    if (!w) return 0;
    if (!inklow_wl_state(w, NULL, NULL) || !pixels || !w->surface || !w->panel_surface ||
        width <= 0 || height <= 0 || width > 16384 || height > 16384 ||
        stride < width * 4 || stride % 4 || scale <= 0 || scale > 16 || width % scale || height % scale ||
        (size_t)stride * (size_t)height > MAX_IMAGE ||
        (scale != 1 && wl_surface_get_version(w->surface) < WL_SURFACE_SET_BUFFER_SCALE_SINCE_VERSION)) {
        inklow_wl_hide(w);
        return 0;
    }
    struct image_buffer *b = NULL;
    for (size_t i = 0; i < 2; ++i) {
        if (!w->buffers[i].busy) {
            b = &w->buffers[i];
            if (b->buffer && b->width == width && b->height == height && b->stride == stride) break;
        }
    }
    if (!b) return 2;
    if (!b->buffer || b->width != width || b->height != height || b->stride != stride) {
        destroy_buffer(b);
        if (!allocate_buffer(w, b, width, height, stride)) { inklow_wl_hide(w); return 0; }
    }
    memcpy(b->data, pixels, b->size);
    b->erase = false;
    if (wl_surface_get_version(w->surface) >= WL_SURFACE_SET_BUFFER_SCALE_SINCE_VERSION)
        wl_surface_set_buffer_scale(w->surface, scale);
    wl_surface_attach(w->surface, b->buffer, 0, 0);
    wl_surface_damage(w->surface, 0, 0, width / scale, height / scale);
    b->busy = true;
    w->visible = true;
    wl_surface_commit(w->surface);
    return inklow_wl_flush(w) >= 0;
}

void inklow_wl_close(struct inklow_wl *w)
{
    if (!w) return;
    /* No user callbacks during destruction. */
    w->callback = NULL;
    release_context(w);
    if (w->pointer) {
        if (wl_pointer_get_version(w->pointer) >= WL_POINTER_RELEASE_SINCE_VERSION) wl_pointer_release(w->pointer);
        else wl_pointer_destroy(w->pointer);
    }
    if (w->panel_surface) zwp_input_panel_surface_v1_destroy(w->panel_surface);
    if (w->surface) wl_surface_destroy(w->surface);
    for (size_t i = 0; i < 2; ++i) destroy_buffer(&w->buffers[i]);
    if (w->panel) zwp_input_panel_v1_destroy(w->panel);
    if (w->method) zwp_input_method_v1_destroy(w->method);
    if (w->seat) {
        if (wl_seat_get_version(w->seat) >= WL_SEAT_RELEASE_SINCE_VERSION) wl_seat_release(w->seat);
        else wl_seat_destroy(w->seat);
    }
    if (w->shm) wl_shm_destroy(w->shm);
    if (w->compositor) wl_compositor_destroy(w->compositor);
    if (w->registry) wl_registry_destroy(w->registry);
    if (w->xkb) xkb_context_unref(w->xkb);
    if (w->display) { wl_display_flush(w->display); wl_display_disconnect(w->display); }
    free(w->text);
    wipe(w, sizeof(*w));
    free(w);
}
