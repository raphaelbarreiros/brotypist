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
struct inklow_wl_event {
    int type;
    uint32_t generation, serial, time, key, state, modifiers;
    int32_t x, y;
    const char *name;
};
typedef int (*inklow_wl_callback)(void *, const struct inklow_wl_event *);

/* Single-threaded, same event-loop ownership. Callbacks must not close the client.
 * Key callback names are borrowed until return; no event contains field text.
 * Canonical modifiers: Shift=1, Control=4, Alt=8, Super=64, unsupported=128.
 * Event 5 reports a reusable compositor buffer; retry only a pending frame.
 */
INKLOW_WL_API inklow_wl *inklow_wl_connect(int owned_fd, inklow_wl_callback, void *userdata);
INKLOW_WL_API int inklow_wl_fd(inklow_wl *);
/* dispatch/flush: 1 drained, 0 pending writes (EAGAIN), -1 disconnected.
 * Watch writable readiness while pending, then call flush again.
 */
INKLOW_WL_API int inklow_wl_dispatch(inklow_wl *);
INKLOW_WL_API int inklow_wl_flush(inklow_wl *);
INKLOW_WL_API void inklow_wl_close(inklow_wl *);
INKLOW_WL_API int inklow_wl_state(inklow_wl *, uint32_t *generation, uint32_t *serial);
/* Stable context lifetime, independent of text/commit revision invalidation. */
INKLOW_WL_API uint64_t inklow_wl_context(inklow_wl *);
INKLOW_WL_API int inklow_wl_matches(inklow_wl *, uint32_t generation, uint32_t serial,
                                   const char *utf8_text, size_t length, size_t cursor_bytes);
INKLOW_WL_API int inklow_wl_commit(inklow_wl *, uint32_t generation, uint32_t serial,
                                  const char *utf8_text);
/* present: 1 attached, 2 buffers busy (retry on event 5), 0 unavailable. */
INKLOW_WL_API int inklow_wl_present(inklow_wl *, const void *argb32, int width,
                                   int height, int stride, int scale);
INKLOW_WL_API void inklow_wl_hide(inklow_wl *);

#ifdef __cplusplus
}
#endif
#endif
