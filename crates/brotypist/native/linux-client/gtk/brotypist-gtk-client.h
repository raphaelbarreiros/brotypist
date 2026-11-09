// SPDX-License-Identifier: MIT
#pragma once
#include <gtk/gtk.h>
#include <xkbcommon/xkbcommon-compose.h>

// This is a source-level hook contract, not an installed Fcitx/GTK ABI.
// All hooks run on the GTK owner loop. Chooser Present retains its authenticated
// invocation until native allocation and after-paint prove the compact rows;
// focus loss, revoke, rebind and destruction cancel that invocation exactly once.
// The chooser's pending allocation does not replace the independent preedit lane.
// Owned-smoke withdrawal diagnostics expose only a fixed local invalidation cause
// and allocation/IM-state booleans; they never renew or relax the witness/lease.
struct BtGtkClient;
// Distinguish source hooks without accepting peer text or IM names as causes.
enum class BtGtkRevoke {
    Availability, Disconnected, ClientUi, CurrentIm, RemotePreedit, UsePreedit,
    Reset, SlavePreedit, RemoteCommit, DeleteSurrounding, Connected, Purpose,
    Hints, SimpleCommit
};
BtGtkClient *bt_gtk_new(GtkIMContext *context, GtkIMContext *simple);
void bt_gtk_free(BtGtkClient *client);
void bt_gtk_window(BtGtkClient *client, GdkWindow *window);
void bt_gtk_focus(BtGtkClient *client, bool focused);
void bt_gtk_revoke(BtGtkClient *client, BtGtkRevoke cause);
void bt_gtk_state(BtGtkClient *client, xkb_compose_state *compose,
                  bool remote_preedit, bool remote_valid, bool use_preedit,
                  bool ordered_batch, bool cached_user_preedit);
void bt_gtk_pending(BtGtkClient *client, int delta);
void bt_gtk_simple_begin(BtGtkClient *client, GdkEventKey *event);
void bt_gtk_simple_commit(BtGtkClient *client);
void bt_gtk_simple_end(BtGtkClient *client, bool consumed);
void bt_gtk_simple_idle(BtGtkClient *client);
void bt_gtk_simple_active(BtGtkClient *client);
void bt_gtk_reset_complete(BtGtkClient *client);
bool bt_gtk_key(BtGtkClient *client, GdkEventKey *event);
bool bt_gtk_preedit(BtGtkClient *client, gchar **text,
                    PangoAttrList **attrs, gint *cursor);
