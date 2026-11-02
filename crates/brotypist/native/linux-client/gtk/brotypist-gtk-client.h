// SPDX-License-Identifier: MIT
#pragma once
#include <gtk/gtk.h>
#include <xkbcommon/xkbcommon-compose.h>

// This is a source-level hook contract, not an installed Fcitx/GTK ABI.
struct BtGtkClient;
BtGtkClient *bt_gtk_new(GtkIMContext *context, GtkIMContext *simple);
void bt_gtk_free(BtGtkClient *client);
void bt_gtk_window(BtGtkClient *client, GdkWindow *window);
void bt_gtk_focus(BtGtkClient *client, bool focused);
void bt_gtk_revoke(BtGtkClient *client);
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
