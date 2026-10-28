/**
 * @file
 * Private Wayland input-method and manual input-panel ABI.
 *
 * All calls and callbacks belong to one event-loop thread. The client owns its
 * Wayland objects and shared-memory buffers; callers retain their text/pixels.
 * Surrounding field text stays native and is exposed only through comparison.
 */
#ifndef INKLOW_WAYLAND_CLIENT_H
#define INKLOW_WAYLAND_CLIENT_H

#include <stddef.h>
#include <stdint.h>

#if defined(__GNUC__)
#define INKLOW_WL_API __attribute__((visibility("default")))
#else
#define INKLOW_WL_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct inklow_wl inklow_wl;
/**
 * Borrowed callback payload, valid only until callback return.
 * Types: 1 context state, 2 key, 3 panel pointer button, 4 transport failure,
 * 5 panel buffer release. Key events use XKB keycodes (evdev + 8); button
 * events use Linux button codes. time is the compositor millisecond timestamp;
 * x/y are panel-local logical coordinates. Type 2 carries the key protocol
 * serial; state/panel/buffer notifications carry the current context serial.
 */
struct inklow_wl_event {
    int type;
    uint32_t generation, serial, time, key, state, modifiers;
    int32_t x, y;
    const char *name;
};
/** Nonzero consumes a type-2 key press and its release; other returns are ignored. */
typedef int (*inklow_wl_callback)(void *, const struct inklow_wl_event *);

/* Single-threaded, same event-loop ownership. Callbacks must not close the client.
 * Key callback names are borrowed until return; no event contains field text.
 * Canonical modifiers: Shift=1, Control=4, Alt=8, Super=64, unsupported=128.
 * Event 1 state: 1 missing initial purpose; 2 known-safe initial state incomplete.
 * Ready and known-sensitive contexts use 0; no field text crosses this event.
 * Event 5 reports a reusable compositor buffer; retry only a pending frame.
 */
/**
 * Consume owned_fd, a connected Unix stream socket, even on failure.
 * Initialization performs blocking roundtrips and may invoke callback before
 * returning. userdata is borrowed for the client lifetime; callback may be NULL.
 * @return Owned client, or NULL when the private input-method setup fails.
 */
INKLOW_WL_API inklow_wl *inklow_wl_connect(int owned_fd, inklow_wl_callback, void *userdata);
/** Borrow the private display descriptor for readiness watches; -1 if unavailable. */
INKLOW_WL_API int inklow_wl_fd(inklow_wl *);
/* dispatch/flush: 1 drained, 0 pending writes (EAGAIN), -1 disconnected.
 * Watch writable readiness while pending, then call flush again.
 */
INKLOW_WL_API int inklow_wl_dispatch(inklow_wl *);
INKLOW_WL_API int inklow_wl_flush(inklow_wl *);
/** Destroy the client and its transport without callbacks; NULL is accepted. */
INKLOW_WL_API void inklow_wl_close(inklow_wl *);
/**
 * Return 1 for a usable committed context, otherwise 0.
 * Optional generation/serial outputs are written only on success and must both
 * accompany later matches/commit calls to reject stale snapshots.
 */
INKLOW_WL_API int inklow_wl_state(inklow_wl *, uint32_t *generation, uint32_t *serial);
/** Return the nonzero context lifetime ID, or 0 when absent/disconnected.
 * Text revisions change generation instead; this ID alone does not prove readiness. */
INKLOW_WL_API uint64_t inklow_wl_context(inklow_wl *);
/**
 * Compare the native excerpt around its cursor with caller-provided field text.
 * length and cursor_bytes are UTF-8 byte counts, not character offsets.
 * Input is borrowed for this call and need not be NUL-terminated.
 * @return 1 on a current matching excerpt; 0 on mismatch, invalid input or stale state.
 */
INKLOW_WL_API int inklow_wl_matches(inklow_wl *, uint32_t generation, uint32_t serial,
                                   const char *utf8_text, size_t length, size_t cursor_bytes);
/**
 * Queue a NUL-terminated UTF-8 insertion of at most 4079 bytes, then invalidate
 * the old snapshot. Returns 0 on refusal/transport failure, 1 when queued or
 * flushed; success is not acknowledgement that the host inserted the text.
 */
INKLOW_WL_API int inklow_wl_commit(inklow_wl *, uint32_t generation, uint32_t serial,
                                  const char *utf8_text);
/**
 * Copy stride * height bytes of premultiplied ARGB8888 into an owned panel buffer.
 * width/height are pixels, stride is bytes, scale is an integer pixel/logical
 * ratio (1..16); pixel dimensions must be divisible by scale.
 * @return 1 queued, 2 both buffers busy (retry on event 5), 0 unavailable.
 */
INKLOW_WL_API int inklow_wl_present(inklow_wl *, const void *argb32, int width,
                                   int height, int stride, int scale);
/** Unmap the panel; erase idle pixel buffers now and busy ones on release. */
INKLOW_WL_API void inklow_wl_hide(inklow_wl *);

#ifdef __cplusplus
}
#endif
#endif
