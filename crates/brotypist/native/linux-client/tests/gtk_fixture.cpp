// Real GtkEntry IM path; the reviewed patched im-fcitx5-gtk3 supplies the client.
#include <gtk/gtk.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <utility>
#include <unistd.h>

struct Fixture {
    GtkWidget *host = nullptr, *box = nullptr, *first = nullptr, *second = nullptr;
    unsigned inserts = 0;
    std::string input;
    gulong cancellation_handler = 0;
    std::string cancellation, cancelled;
    guint connected_signal = 0;
    gulong connected_hook = 0, initial_im_handler = 0, startup_paint_handler = 0;
    GObject *initial_client = nullptr;
    GdkFrameClock *startup_clock = nullptr;
    bool startup_ready = false, readiness_requested = false;
    void observe_initial_connection() {
        // Only the actual first entry exists here. Its private module has been
        // instantiated by realization, but gtk_main has not dispatched its
        // asynchronous connection yet. No spare/probe IM context is created.
        const GType type = g_type_from_name("FcitxGClient");
        connected_signal = type ? g_signal_lookup("connected", type) : 0;
        if (!connected_signal) {
            std::fputs("GTK fixture: actual Fcitx client signal unavailable\n", stderr);
            std::exit(7);
        }
        connected_hook = g_signal_add_emission_hook(connected_signal, 0,
            +[](GSignalInvocationHint *, guint count, const GValue *values, gpointer data)->gboolean {
                auto &self = *static_cast<Fixture *>(data);
                if (count != 1 || self.initial_client || self.second) std::exit(7);
                self.initial_client = G_OBJECT(g_value_dup_object(values));
                self.connected_hook = 0; // returning FALSE removes this one-shot hook
                // The source's connected callback still has to update readiness
                // and FocusIn. Wait for this SAME client's initial IM selection,
                // after its existing native handler, not an unrelated client.
                self.initial_im_handler = g_signal_connect_after(self.initial_client, "current-im",
                    G_CALLBACK(+[](GObject *client, const char *, const char *, const char *, gpointer data) {
                        auto &self = *static_cast<Fixture *>(data);
                        g_signal_handler_disconnect(client, self.initial_im_handler);
                        self.initial_im_handler = 0;
                        if (!self.first || !gtk_widget_has_focus(self.first) || self.second) std::exit(7);
                        self.second = gtk_entry_new();
                        gtk_entry_set_text(GTK_ENTRY(self.second), "other");
                        gtk_box_pack_start(GTK_BOX(self.box), self.second, FALSE, FALSE, 12);
                        auto *clock = gtk_widget_get_frame_clock(self.host);
                        if (!clock) std::exit(7);
                        self.startup_clock = GDK_FRAME_CLOCK(g_object_ref(clock));
                        self.startup_paint_handler = g_signal_connect_after(clock, "after-paint",
                            G_CALLBACK(+[](GdkFrameClock *clock, gpointer data) {
                                auto &self = *static_cast<Fixture *>(data);
                                g_signal_handler_disconnect(clock, self.startup_paint_handler);
                                self.startup_paint_handler = 0;
                                if (!gtk_widget_has_focus(self.first) ||
                                    !gtk_widget_get_mapped(self.first) || !gtk_widget_get_mapped(self.second))
                                    std::exit(7);
                                g_clear_object(&self.startup_clock);
                                g_clear_object(&self.initial_client);
                                self.startup_ready = true;
                                if (self.readiness_requested) self.state();
                            }), &self);
                        gtk_widget_show_all(self.host);
                        gtk_widget_queue_resize(self.host);
                    }), &self);
                return FALSE;
            }, this, nullptr);
        if (!connected_hook) std::exit(7);
    }
    static void visit(GtkWidget *widget, gpointer data) {
        auto *buttons = static_cast<GPtrArray *>(data);
        if (GTK_IS_BUTTON(widget) && gtk_widget_get_mapped(widget)) {
            auto *ancestor = gtk_widget_get_ancestor(widget, GTK_TYPE_POPOVER);
            if (ancestor) g_ptr_array_add(buttons, widget);
        }
        if (GTK_IS_CONTAINER(widget)) gtk_container_forall(GTK_CONTAINER(widget), visit, data);
    }
    GPtrArray *buttons() {
        auto *result = g_ptr_array_new();
        visit(host, result);
        return result;
    }
    static std::string hex(const char *value) {
        std::string result;
        for (const auto *p = reinterpret_cast<const unsigned char *>(value); *p; ++p) {
            result += "0123456789abcdef"[*p >> 4]; result += "0123456789abcdef"[*p & 15];
        }
        return result;
    }
    void state() {
        auto *rows = buttons();
        std::printf("%s\t%d\t%s\t%u\t0\t%u\n",
            first ? hex(gtk_entry_get_text(GTK_ENTRY(first))).c_str() : "destroyed",
            first ? gtk_editable_get_position(GTK_EDITABLE(first)) : -1,
            hex(gtk_entry_get_text(GTK_ENTRY(second))).c_str(), inserts, rows->len);
        std::fflush(stdout); g_ptr_array_unref(rows);
    }
    void geometry() {
        if (!first || !gtk_widget_get_window(host)) std::exit(5);
        auto *monitor = gdk_display_get_monitor_at_window(gtk_widget_get_display(host),
            gtk_widget_get_window(host));
        if (!monitor) std::exit(5);
        GdkRectangle available;
        gdk_monitor_get_workarea(monitor, &available);
        // Measure real native requisitions in an owned, unexposed popover with
        // the same widget ancestry/style. Never alter fonts or desktop geometry.
        auto *probe = gtk_popover_new(first);
        auto *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
        gtk_container_add(GTK_CONTAINER(probe), box);
        const std::string labels[] = {"2  " + std::string(160, 'W'), "1  first", "2  third"};
        int widths[3] = {};
        for (unsigned i = 0; i < 3; ++i) {
            auto *button = gtk_button_new_with_label(labels[i].c_str());
            gtk_box_pack_start(GTK_BOX(box), button, FALSE, FALSE, 0);
            gtk_widget_show(button);
            int minimum = 0;
            gtk_widget_get_preferred_width(button, &minimum, &widths[i]);
        }
        gtk_widget_destroy(probe);
        std::printf("geometry\t%d\t%d\t%d\t%d\n", widths[0], available.width, widths[1], widths[2]);
        std::fflush(stdout);
    }
    void cancel_choice(const char *kind) {
        auto *clock = gtk_widget_get_frame_clock(host);
        if (!clock || cancellation_handler) std::exit(6);
        cancellation = kind; cancelled.clear();
        cancellation_handler = g_signal_connect(clock, "before-paint", G_CALLBACK(+[](GdkFrameClock *clock, gpointer data) {
            auto &self = *static_cast<Fixture *>(data);
            auto *rows = self.buttons();
            if (self.cancellation == "row-destroy") {
                // Observe the real overwide candidate, then invalidate focus
                // synchronously from its rejection/destruction, not a timer.
                const std::string rejected = "2  " + std::string(160, 'W');
                GtkWidget *row = nullptr;
                for (guint i = 0; i < rows->len; ++i) {
                    auto *button = GTK_WIDGET(g_ptr_array_index(rows, i));
                    if (rejected == gtk_button_get_label(GTK_BUTTON(button))) row = button;
                }
                if (row) {
                    g_signal_handler_disconnect(clock, self.cancellation_handler);
                    self.cancellation_handler = 0;
                    g_signal_connect(row, "destroy", G_CALLBACK(+[](GtkWidget *, gpointer data) {
                        auto &self = *static_cast<Fixture *>(data);
                        self.cancelled = std::move(self.cancellation);
                        gtk_widget_grab_focus(self.second);
                    }), &self);
                }
                g_ptr_array_unref(rows);
                return;
            }
            const bool mapped = rows->len != 0;
            g_ptr_array_unref(rows);
            if (!mapped) return;
            g_signal_handler_disconnect(clock, self.cancellation_handler);
            self.cancellation_handler = 0;
            self.cancelled = std::move(self.cancellation);
            if (self.cancelled == "focus") gtk_widget_grab_focus(self.second);
            else gtk_widget_set_margin_start(self.first, 24);
        }), this);
    }
    void execute(const std::string &command) {
        if (command == "ready") {
            if (readiness_requested) std::exit(7);
            readiness_requested = true;
            if (startup_ready) state();
            return;
        }
        // The harness bounds the single ready response. Never turn a timeout
        // into presumed connection readiness or allow presentation probes first.
        if (!startup_ready) std::exit(7);
        if (command == "geometry") { geometry(); return; }
        if (command == "cancellation") {
            std::printf("cancellation\t%s\n", cancelled.c_str()); std::fflush(stdout); return;
        }
        if (command == "state") {}
        else if (command == "cancel-choice-focus") cancel_choice("focus");
        else if (command == "cancel-choice-layout") cancel_choice("layout");
        else if (command == "cancel-choice-row-destroy") cancel_choice("row-destroy");
        else if (command == "reset" && first) {
            if (cancellation_handler) {
                g_signal_handler_disconnect(gtk_widget_get_frame_clock(host), cancellation_handler);
                cancellation_handler = 0;
            }
            cancellation.clear(); cancelled.clear();
            gtk_widget_set_margin_start(first, 0);
            gtk_entry_set_text(GTK_ENTRY(first), "seed ");
            gtk_entry_set_text(GTK_ENTRY(second), "other");
            gtk_widget_show_all(host); gtk_window_present(GTK_WINDOW(host));
            gtk_widget_grab_focus(first);
            // GtkEntry may select on focus; establish the real collapsed caret
            // after focus delivery, rather than relying on the previous focus.
            gtk_editable_set_position(GTK_EDITABLE(first), 5); inserts = 0;
        } else if (command == "focus2") gtk_widget_grab_focus(second);
        else if (command == "hide") gtk_widget_hide(host);
        else if (command == "show") {
            gtk_widget_show_all(host); gtk_window_present(GTK_WINDOW(host));
            if (first) gtk_widget_grab_focus(first);
        } else if (command == "destroy" && first) { gtk_widget_destroy(first); first = nullptr; }
        else if (command == "choice2" || command == "hide-popup") {
            auto *rows = buttons();
            GtkWidget *chosen = nullptr;
            for (guint i = 0; i < rows->len; ++i) {
                auto *button = GTK_WIDGET(g_ptr_array_index(rows, i));
                if (std::strcmp(gtk_button_get_label(GTK_BUTTON(button)), "2  third") == 0) chosen = button;
            }
            if (chosen) {
                if (command == "choice2") gtk_button_clicked(GTK_BUTTON(chosen));
                else gtk_widget_hide(gtk_widget_get_ancestor(chosen, GTK_TYPE_POPOVER));
            }
            g_ptr_array_unref(rows);
        } else std::exit(4);
        state();
    }
    Fixture() {
        host = gtk_window_new(GTK_WINDOW_TOPLEVEL);
        gtk_window_set_title(GTK_WINDOW(host), "BroTypist owned synthetic GTK fixture");
        gtk_window_set_default_size(GTK_WINDOW(host), 720, 220);
        box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 12);
        gtk_container_add(GTK_CONTAINER(host), box);
        first = gtk_entry_new();
        gtk_entry_set_text(GTK_ENTRY(first), "seed ");
        gtk_box_pack_start(GTK_BOX(box), first, FALSE, FALSE, 12);
        g_signal_connect(first, "insert-text", G_CALLBACK(+[](GtkEditable *, gchar *, gint, gint *, gpointer p) {
            ++static_cast<Fixture *>(p)->inserts;
        }), this);
        g_signal_connect(host, "destroy", G_CALLBACK(gtk_main_quit), nullptr);
        auto *channel = g_io_channel_unix_new(STDIN_FILENO);
        g_io_add_watch(channel, static_cast<GIOCondition>(G_IO_IN | G_IO_HUP), +[](GIOChannel *, GIOCondition condition, gpointer data)->gboolean {
            if (condition & G_IO_HUP) { gtk_main_quit(); return G_SOURCE_REMOVE; }
            auto &self = *static_cast<Fixture *>(data);
            char bytes[128]; const auto n = ::read(STDIN_FILENO, bytes, sizeof bytes);
            if (n <= 0) { gtk_main_quit(); return G_SOURCE_REMOVE; }
            self.input.append(bytes, size_t(n));
            if (self.input.size() > 256) std::exit(3);
            size_t end;
            while ((end = self.input.find('\n')) != std::string::npos) {
                const auto command = self.input.substr(0, end); self.input.erase(0, end + 1);
                self.execute(command);
            }
            return G_SOURCE_CONTINUE;
        }, this);
        g_io_channel_unref(channel);
        gtk_widget_show_all(host); gtk_window_present(GTK_WINDOW(host));
        gtk_widget_grab_focus(first); gtk_editable_set_position(GTK_EDITABLE(first), 5);
        observe_initial_connection();
    }
    ~Fixture() {
        if (connected_hook) g_signal_remove_emission_hook(connected_signal, connected_hook);
        if (initial_im_handler) g_signal_handler_disconnect(initial_client, initial_im_handler);
        if (startup_paint_handler) g_signal_handler_disconnect(startup_clock, startup_paint_handler);
        g_clear_object(&startup_clock);
        g_clear_object(&initial_client);
    }
};
int main(int argc, char **argv) {
    if (geteuid() == 0 || g_strcmp0(g_getenv("BROTYPIST_OWNED_CLIENT_SMOKE"), "1") ||
        !g_getenv("BROTYPIST_NATIVE_BUS") || g_strcmp0(g_getenv("GTK_IM_MODULE"), "fcitx")) return 2;
    gtk_init(&argc, &argv);
    Fixture fixture; gtk_main(); return 0;
}
