// SPDX-License-Identifier: MIT
#include "brotypist-gtk-client.h"
#include <gio/gio.h>
#include <unistd.h>
#include <algorithm>
#include <cstring>
#include <string>
#include <vector>
#include <utility>

namespace {
using Dict = GVariantBuilder;
constexpr guint64 lease = 2000000000ULL;
guint64 now() { return guint64(g_get_monotonic_time()) * 1000; }
void init(Dict &d) { g_variant_builder_init(&d, G_VARIANT_TYPE_VARDICT); }
void str(Dict &d, const char *k, const std::string &v) { g_variant_builder_add(&d, "{sv}", k, g_variant_new_string(v.c_str())); }
void num(Dict &d, const char *k, guint64 v) { g_variant_builder_add(&d, "{sv}", k, g_variant_new_uint64(v)); }
void u32(Dict &d, const char *k, guint v) { g_variant_builder_add(&d, "{sv}", k, g_variant_new_uint32(v)); }
void flag(Dict &d, const char *k, bool v) { g_variant_builder_add(&d, "{sv}", k, g_variant_new_boolean(v)); }
std::string sget(GVariant *d, const char *k) { const char *v = ""; if (d) g_variant_lookup(d, k, "&s", &v); return v; }
guint64 nget(GVariant *d, const char *k) { guint64 v = 0; if (d) g_variant_lookup(d, k, "t", &v); return v; }
guint uget(GVariant *d, const char *k) { guint v = 0; if (d) g_variant_lookup(d, k, "u", &v); return v; }
bool bget(GVariant *d, const char *k) { gboolean v = FALSE; if (d) g_variant_lookup(d, k, "b", &v); return v; }
bool typed(GVariant *d, const char *key, const GVariantType *type) {
    GVariant *v = d ? g_variant_lookup_value(d,key,type) : nullptr;
    if (!v) return false;
    g_variant_unref(v); return true;
}
bool complete_offer(GVariant *d) {
    for (const char *k : {"offer_id","revision","user_revision","policy_epoch","expires_ns"})
        if (!typed(d,k,G_VARIANT_TYPE_UINT64)) return false;
    for (const char *k : {"mode","field_token","window_token","document_token","text_sha256","offset_unit","text"})
        if (!typed(d,k,G_VARIANT_TYPE_STRING)) return false;
    for (const char *k : {"caret","anchor","start","length"})
        if (!typed(d,k,G_VARIANT_TYPE_UINT32)) return false;
    return typed(d,"values",G_VARIANT_TYPE_STRING_ARRAY) && nget(d,"offer_id");
}
std::string token(GObject *o) {
    if (!o) return {};
    const char *p = static_cast<const char *>(g_object_get_data(o, "brotypist-native-token"));
    if (!p) { g_object_set_data_full(o, "brotypist-native-token", g_uuid_string_random(), g_free); p = static_cast<const char *>(g_object_get_data(o, "brotypist-native-token")); }
    return p;
}
std::string sha(const std::string &s) { char *v = g_compute_checksum_for_data(G_CHECKSUM_SHA256, reinterpret_cast<const guchar *>(s.data()), s.size()); std::string r(v); g_free(v); return r; }
bool bounded(const std::string &s) { return s.size() <= 65536 && g_utf8_validate(s.c_str(), s.size(), nullptr) && g_utf8_strlen(s.c_str(), s.size()) <= 16000; }
std::string slice(const std::string &s, guint start, guint count) {
    const char *p = g_utf8_offset_to_pointer(s.c_str(),start);
    const char *end = g_utf8_offset_to_pointer(p,count);
    return std::string(p,size_t(end-p));
}
struct Snapshot {
    std::string field, window, document, purpose = "unknown", composing = "unknown", text;
    guint64 revision = 0, user = 0, notification = 0;
    guint caret = 0, anchor = 0;
    bool focus = false, protected_ = true, editable = false;
    GVariant *dict(bool content) const {
        Dict d; init(d); str(d,"field_token",field); str(d,"window_token",window); str(d,"document_token",document);
        str(d,"purpose",purpose); str(d,"composing",composing); str(d,"domain",""); str(d,"offset_unit","scalar");
        num(d,"revision",revision); num(d,"user_revision",user); num(d,"notification_revision",notification);
        flag(d,"focus",focus); flag(d,"protected",protected_); flag(d,"editable",editable);
        if (content) { str(d,"text",text); u32(d,"caret",caret); u32(d,"anchor",anchor); }
        return g_variant_builder_end(&d);
    }
};
struct Bridge;
struct Keep {
    gpointer object;
    explicit Keep(gpointer p) : object(p ? g_object_ref(p) : nullptr) {}
    Keep(const Keep &) = delete;
    Keep &operator=(const Keep &) = delete;
    ~Keep() { if (object) g_object_unref(object); }
};
Bridge &bridge();
}

struct BtGtkPresentation {
    BtGtkClient &owner;
    GVariant *offer = nullptr;
    guint64 capability = 0, deadline = 0;
    bool armed = false, clearing = false;
    std::string armed_word, armed_all, ghost;
    gint ghost_cursor = 0;
    GtkWidget *popover = nullptr;
    std::vector<std::string> values;
    std::vector<GtkWidget *> buttons; // owned by popover, original prefix indices
    guint choice_cursor = 0, expiry_source = 0;
    explicit BtGtkPresentation(BtGtkClient &c) : owner(c) {}
    void clear();
    bool ghost_visible();
    bool textview_ghost_visible();
    bool choice_visible(guint index);
    void choose(guint index);
    GVariant *present(GVariant *o);
    bool arm(GVariant *authorization);
    bool accept(const char *action, guint index = 0);
};

struct BtGtkClient {
    GtkIMContext *context; // source owner outlives this object
    GtkWidget *widget = nullptr;
    GdkWindow *native_window = nullptr;
    GtkTextBuffer *buffer = nullptr;
    xkb_compose_state *compose = nullptr;
    bool focused = false, remote = false, remote_preedit = false, use_preedit = false;
    bool remote_ordered = false, cached_preedit = false;
    enum Simple { Idle, Active, Unknown } simple_state = Idle;
    bool simple_resolved = false, simple_empty_before = false, simple_ordinary = false;
    bool destroying = false;
    bool simple_committed = false, simple_started = false, editing = false, clearing = false;
    unsigned pending = 0, simple_depth = 0;
    guint64 revision = 1, user_revision = 0, notification_revision = 0;
    std::string signature;
    guint changed_source = 0;
    BtGtkPresentation inline_offer, chooser_offer;
    explicit BtGtkClient(GtkIMContext *c) : context(c), inline_offer(*this), chooser_offer(*this) {}
    ~BtGtkClient();
    void clear(guint64 capability = 0);
    void unbind();
    void bind(GdkWindow *window);
    Snapshot metadata();
    bool capture(Snapshot &out, const char *&reason, bool owned_readback = false);
    void changed(bool user = false);
    void notify();
    bool witness(GVariant *o, Snapshot &s, const char *&reason);
    GVariant *present(GVariant *o);
    bool arm(GVariant *authorization);
    void select(guint caret, guint anchor);
};

namespace {
struct Bridge {
    GDBusConnection *bus = nullptr;
    GVariant *policy = nullptr;
    BtGtkClient *active = nullptr;
    std::string name, owner, path;
    guint watch = 0, object = 0;
    guint64 id = 0, serial = 0, acceptance = 0, generation = 0;
    guint64 focus_generation = 1;
    std::string scope_field, scope_window, scope_document;
    bool scope_focus = false, scope_known = false;
    unsigned outstanding = 0;
    bool registering = false;
    bool pending_edit = false, pending_quarantine = false, deferred_changed = false;
    GVariant *pending_receipt = nullptr;
    guint64 pending_ns = 0, pending_generation = 0, pending_client_id = 0;
    std::string pending_owner;
    bool live() const { return id && policy && nget(policy,"policy_epoch") && nget(policy,"expires_ns") > now(); }
    bool enabled() const { return live() && bget(policy,"enabled"); }
    void expire_policy() {
        if (policy) { g_variant_unref(policy); policy = nullptr; }
        if (active) { Keep lifetime(active->context); active->clear(); }
    }
    bool observe_scope(const Snapshot &s) {
        if (scope_known && scope_field == s.field && scope_window == s.window &&
            scope_document == s.document && scope_focus == s.focus) return false;
        scope_known = true; scope_field = s.field; scope_window = s.window;
        scope_document = s.document; scope_focus = s.focus;
        ++focus_generation; expire_policy(); return true;
    }
    bool configure(GVariant *p) {
        Keep context_lifetime(active ? active->context : nullptr);
        // Inert withdrawal always works, including across an old target scope.
        if (!nget(p,"expires_ns")) { expire_policy(); return true; }
        Snapshot s = active ? active->metadata() : Snapshot();
        if (!s.focus || !nget(p,"policy_epoch") || nget(p,"expires_ns") <= now() ||
            nget(p,"focus_generation") != focus_generation ||
            !typed(p,"field_token",G_VARIANT_TYPE_STRING) ||
            !typed(p,"window_token",G_VARIANT_TYPE_STRING) ||
            !typed(p,"document_token",G_VARIANT_TYPE_STRING) ||
            sget(p,"field_token") != s.field || sget(p,"window_token") != s.window ||
            sget(p,"document_token") != s.document) {
            expire_policy(); return false;
        }
        bool same = policy && nget(p,"expires_ns") >= nget(policy,"expires_ns");
        if (same) {
            for (GVariant *dict : {policy,p}) {
                GVariantIter it; const char *key; GVariant *value; g_variant_iter_init(&it,dict);
                while (g_variant_iter_next(&it,"{&sv}",&key,&value)) {
                    GVariant *other = g_variant_lookup_value(dict == policy ? p : policy,key,nullptr);
                    if (std::strcmp(key,"expires_ns") && (!other || !g_variant_equal(value,other))) same = false;
                    if (other) g_variant_unref(other);
                    g_variant_unref(value);
                }
            }
        }
        if (!same && active) active->clear();
        if (policy) g_variant_unref(policy);
        policy = g_variant_ref(p); // never extend an existing offer's own deadline
        return true;
    }
    void revoke() { ++generation; registering = false; id = 0; owner.clear(); if (active) active->clear(); if (policy) { g_variant_unref(policy); policy = nullptr; } }
    void event(GVariant *event) {
        if (!id || !bus || owner.empty() || outstanding >= 32) { g_variant_ref_sink(event); g_variant_unref(event); if (outstanding >= 32) revoke(); return; }
        ++outstanding;
        auto *request = new std::pair<Bridge *,guint64>(this,generation);
        g_dbus_connection_call(bus, owner.c_str(), "/org/brotypist/Native1", "org.brotypist.Native1", "Event", g_variant_new("(t@a{sv})", id, event), G_VARIANT_TYPE("()"), G_DBUS_CALL_FLAGS_NONE, 1500, nullptr,
            [](GObject *o, GAsyncResult *r, gpointer data) {
                auto *request = static_cast<std::pair<Bridge *,guint64> *>(data); auto &b = *request->first;
                GError *e = nullptr; GVariant *v = g_dbus_connection_call_finish(G_DBUS_CONNECTION(o),r,&e); --b.outstanding;
                if (v) g_variant_unref(v);
                if (e) { g_error_free(e); if (b.generation == request->second) b.revoke(); }
                delete request;
            }, request);
    }
    void begin_edit(GVariant *frozen, guint64 dispatch_ns) {
        pending_edit = true; pending_quarantine = false; deferred_changed = true;
        pending_ns = dispatch_ns; pending_generation = generation;
        pending_client_id = id; pending_owner = owner;
        pending_receipt = g_variant_ref_sink(frozen);
    }
    void finish_edit(GVariant *terminal) {
        // This record belongs to the process, not the focused widget. Retain it
        // on focus changes, owner loss or failed/late ACK; never expose the
        // unconfirmed generated output as newly authored text.
        if (pending_receipt) g_variant_unref(pending_receipt);
        pending_receipt = g_variant_ref_sink(terminal);
        guint64 deadline = pending_ns + lease, send_ns = now();
        if (!bus || !id || pending_client_id != id || pending_generation != generation ||
            pending_owner != owner || send_ns >= deadline || outstanding >= 32) {
            pending_quarantine = true; expire_policy(); return;
        }
        ++outstanding;
        auto *request = new std::pair<Bridge *,guint64>(this,nget(pending_receipt,"acceptance_id"));
        gint timeout_ms = gint(std::max<guint64>(1,(deadline-send_ns+999999)/1000000));
        g_dbus_connection_call(bus,pending_owner.c_str(),"/org/brotypist/Native1","org.brotypist.Native1","Event",
            g_variant_new("(t@a{sv})",pending_client_id,g_variant_ref(pending_receipt)),
            G_VARIANT_TYPE("()"),G_DBUS_CALL_FLAGS_NONE,timeout_ms,nullptr,
            [](GObject *object,GAsyncResult *result,gpointer data) {
                auto *request = static_cast<std::pair<Bridge *,guint64> *>(data); auto &b = *request->first;
                GError *error = nullptr;
                GVariant *ack = g_dbus_connection_call_finish(G_DBUS_CONNECTION(object),result,&error);
                --b.outstanding;
                bool current = b.pending_edit && b.pending_receipt &&
                    nget(b.pending_receipt,"acceptance_id") == request->second;
                // The broker validates original-dispatch +2s before enqueue.
                // A real successful ACK is conclusive even if GUI callback
                // delivery itself was delayed or the current scope changed.
                bool confirmed = current && ack && !error;
                if (ack) g_variant_unref(ack);
                if (error) g_error_free(error);
                delete request;
                if (!current) return;
                if (!confirmed) { b.pending_quarantine = true; b.expire_policy(); return; }
                g_variant_unref(b.pending_receipt); b.pending_receipt = nullptr;
                b.pending_edit = false; b.pending_quarantine = false;
                bool notify = b.deferred_changed; b.deferred_changed = false;
                if (notify && b.active) b.active->notify(); // Core enqueue is now acknowledged
            },request);
    }
    bool action(const char *a,const Snapshot &origin,guint64 source_generation,guint64 source_epoch) {
        if (!active) return false;
        Snapshot current = active->metadata(); // may expire a newly transitioned scope
        bool toggle = !std::strcmp(a,"toggle") || !std::strcmp(a,"app_toggle");
        if (!(toggle ? live() : enabled()) || !current.focus ||
            source_epoch != nget(policy,"policy_epoch") || source_generation != focus_generation ||
            origin.field != current.field || origin.window != current.window ||
            origin.document != current.document) return false;
        Dict d; init(d); str(d,"kind","action"); str(d,"action",a);
        num(d,"policy_epoch",source_epoch); num(d,"focus_generation",source_generation);
        str(d,"field_token",origin.field); str(d,"window_token",origin.window); str(d,"document_token",origin.document);
        event(g_variant_builder_end(&d)); return true;
    }
    void registration();
    void start();
};
Bridge &bridge() { static Bridge b; return b; }
GVariant *refusal(const char *reason) { Dict d; init(d); flag(d,"presented",false); str(d,"reason",reason); return g_variant_builder_end(&d); }
void method(GDBusConnection *, const gchar *sender, const gchar *, const gchar *, const gchar *name, GVariant *args, GDBusMethodInvocation *call, gpointer data) {
    auto &b = *static_cast<Bridge *>(data);
    if (b.owner.empty() || b.owner != sender || !b.id) { g_dbus_method_invocation_return_dbus_error(call,"org.brotypist.Client1.Denied","peer"); return; }
    Keep context_lifetime(b.active ? b.active->context : nullptr);
    if (!std::strcmp(name,"Configure")) {
        GVariant *p; g_variant_get(args,"(@a{sv})",&p); bool accepted = b.configure(p); g_variant_unref(p);
        if (accepted) g_dbus_method_invocation_return_value(call,g_variant_new("()"));
        else g_dbus_method_invocation_return_dbus_error(call,"org.brotypist.Client1.Scope","scope");
        return;
    }
    if (!std::strcmp(name,"Clear")) { guint64 c; g_variant_get(args,"(t)",&c); if (b.active) b.active->clear(c); g_dbus_method_invocation_return_value(call,g_variant_new("()")); return; }
    if (!std::strcmp(name,"Arm")) {
        GVariant *authorization; g_variant_get(args,"(@a{sv})",&authorization);
        bool armed = b.active && b.active->arm(authorization); g_variant_unref(authorization);
        g_dbus_method_invocation_return_value(call,g_variant_new("(b)",armed)); return;
    }
    GVariant *result = nullptr;
    if (!std::strcmp(name,"Metadata")) {
        Snapshot s = b.active ? b.active->metadata() : Snapshot(); Dict d; init(d);
        u32(d,"protocol",1); str(d,"backend","gtk3-fcitx-client/1"); u32(d,"pid",getpid());
        gchar *exe = g_file_read_link("/proc/self/exe",nullptr); str(d,"app_id",exe ? exe : ""); g_free(exe);
        flag(d,"focus",s.focus); flag(d,"protected",s.protected_); flag(d,"editable",s.editable);
        str(d,"purpose",s.purpose); str(d,"composing",s.composing); str(d,"domain","");
        str(d,"field_token",s.field); str(d,"window_token",s.window); str(d,"document_token",s.document);
        num(d,"focus_generation",b.focus_generation);
        flag(d,"pending_edit",b.pending_edit || (b.active && b.active->editing));
        result = g_variant_builder_end(&d);
    }
    else if (!std::strcmp(name,"Capture")) { guint64 epoch; g_variant_get(args,"(t)",&epoch); Snapshot s; const char *reason = "target"; if (epoch == nget(b.policy,"policy_epoch") && b.active && b.active->capture(s,reason)) result = s.dict(true); else { Dict d; init(d); str(d,"reason",epoch == nget(b.policy,"policy_epoch") ? reason : "policy"); result = g_variant_builder_end(&d); } }
    else if (!std::strcmp(name,"Present")) { GVariant *o; g_variant_get(args,"(@a{sv})",&o); result = b.active ? b.active->present(o) : refusal("target"); g_variant_unref(o); }
    else { g_dbus_method_invocation_return_dbus_error(call,"org.brotypist.Client1.Method","method"); return; }
    g_dbus_method_invocation_return_value(call,g_variant_new("(@a{sv})",result));
}
void Bridge::registration() {
    if (!bus || owner.empty() || registering || id) return;
    Dict d; init(d); u32(d,"protocol",1); str(d,"backend","gtk3-fcitx-client/1"); u32(d,"pid",getpid()); gchar *exe = g_file_read_link("/proc/self/exe",nullptr); str(d,"app_id",exe ? exe : ""); g_free(exe);
    Snapshot s = active ? active->metadata() : Snapshot();
    flag(d,"focus",s.focus); flag(d,"protected",s.protected_); flag(d,"editable",s.editable);
    str(d,"purpose",s.purpose); str(d,"composing",s.composing); str(d,"domain","");
    str(d,"field_token",s.field); str(d,"window_token",s.window); str(d,"document_token",s.document);
    num(d,"focus_generation",focus_generation);
    flag(d,"pending_edit",pending_edit || (active && active->editing));
    registering = true;
    auto *request = new std::pair<Bridge *,guint64>(this,generation);
    g_dbus_connection_call(bus,owner.c_str(),"/org/brotypist/Native1","org.brotypist.Native1","Register",g_variant_new("(o@a{sv})",path.c_str(),g_variant_builder_end(&d)),G_VARIANT_TYPE("(ta{sv})"),G_DBUS_CALL_FLAGS_NONE,1500,nullptr,
        [](GObject *o,GAsyncResult *r,gpointer data) {
            auto *request = static_cast<std::pair<Bridge *,guint64> *>(data); auto &b = *request->first;
            GError *e = nullptr; GVariant *v = g_dbus_connection_call_finish(G_DBUS_CONNECTION(o),r,&e);
            bool current = b.generation == request->second; delete request;
            if (current) b.registering = false;
            if (!v) { if(e) g_error_free(e); return; }
            GVariant *p; guint64 id; g_variant_get(v,"(t@a{sv})",&id,&p);
            if (current && !b.owner.empty()) { b.id = id; b.configure(p); if (b.active) b.active->notify(); }
            g_variant_unref(p); g_variant_unref(v);
        },request);
}
void Bridge::start() {
    if (watch || !g_getenv("BROTYPIST_NATIVE_BUS") || !*g_getenv("BROTYPIST_NATIVE_BUS")) return;
    name = g_getenv("BROTYPIST_NATIVE_BUS"); if (!g_dbus_is_name(name.c_str())) return;
    watch = g_bus_watch_name(G_BUS_TYPE_SESSION,name.c_str(),G_BUS_NAME_WATCHER_FLAGS_NONE,
        [](GDBusConnection *c,const gchar *,const gchar *owner,gpointer data) { auto &b = *static_cast<Bridge *>(data); b.revoke(); if (!b.bus) b.bus = G_DBUS_CONNECTION(g_object_ref(c)); b.owner = owner;
            if (!b.object) { b.path = "/org/brotypist/Client1/p" + std::to_string(getpid());
                static const char xml[] = "<node><interface name='org.brotypist.Client1'><method name='Metadata'><arg type='a{sv}' direction='out'/></method><method name='Capture'><arg type='t' direction='in'/><arg type='a{sv}' direction='out'/></method><method name='Present'><arg type='a{sv}' direction='in'/><arg type='a{sv}' direction='out'/></method><method name='Arm'><arg type='a{sv}' direction='in'/><arg type='b' direction='out'/></method><method name='Clear'><arg type='t' direction='in'/></method><method name='Configure'><arg type='a{sv}' direction='in'/></method></interface></node>";
                GDBusNodeInfo *info = g_dbus_node_info_new_for_xml(xml,nullptr); static const GDBusInterfaceVTable table = {method,nullptr,nullptr,{nullptr}}; b.object = g_dbus_connection_register_object(c,b.path.c_str(),info->interfaces[0],&table,&b,nullptr,nullptr); g_dbus_node_info_unref(info);
            } if (b.object) b.registration(); },
        [](GDBusConnection *,const gchar *,gpointer data) { static_cast<Bridge *>(data)->revoke(); },this,nullptr);
}
void on_changed(GObject *, gpointer data) { static_cast<BtGtkClient *>(data)->changed(); }
void on_notify(GObject *, GParamSpec *pspec, gpointer data) {
    auto *c = static_cast<BtGtkClient *>(data);
    if (c->destroying) return;
    Keep lifetime(c->context);
    const char *name = g_param_spec_get_name(pspec);
    if (!std::strcmp(name,"buffer") && c->widget && GTK_IS_TEXT_VIEW(c->widget)) {
        GdkWindow *w = gtk_text_view_get_window(GTK_TEXT_VIEW(c->widget),GTK_TEXT_WINDOW_TEXT);
        if (w) { g_object_ref(w); c->bind(w); g_object_unref(w); return; }
    }
    for (const char *p : {"cursor-position","selection-bound","visibility","input-purpose","input-hints","editable","sensitive","has-focus","buffer","overwrite-mode","max-length"})
        if (!std::strcmp(name,p)) { c->metadata(); c->changed(); return; }
}
void on_hierarchy(GtkWidget *,GtkWidget *,gpointer data) {
    auto *c = static_cast<BtGtkClient *>(data); if (c->destroying) return;
    Keep lifetime(c->context); c->metadata(); c->changed();
}
void on_mark(GtkTextBuffer *, GtkTextIter *, GtkTextMark *, gpointer data) { static_cast<BtGtkClient *>(data)->changed(); }
gboolean on_button(GtkWidget *,GdkEventButton *,gpointer data) { static_cast<BtGtkClient *>(data)->changed(true); return FALSE; }
void on_destroy(GtkWidget *,gpointer data) {
    auto *c = static_cast<BtGtkClient *>(data); if (c->destroying) return;
    Keep lifetime(c->context); c->focused = false; c->metadata(); c->changed();
}
}

BtGtkClient::~BtGtkClient() {
    destroying = true;
    auto &b = bridge();
    if (b.active == this) {
        b.active = nullptr; b.scope_known = false; ++b.focus_generation; b.expire_policy();
        Dict d; init(d); str(d,"kind","changed"); str(d,"field_token",""); str(d,"window_token",""); str(d,"document_token","");
        num(d,"revision",++revision); num(d,"user_revision",user_revision); num(d,"notification_revision",++notification_revision);
        flag(d,"focus",false); flag(d,"protected",true); str(d,"composing","unknown"); b.event(g_variant_builder_end(&d));
    }
    if (changed_source) g_source_remove(changed_source);
    clear(); unbind();
}
void BtGtkClient::unbind() {
    if (buffer) { g_signal_handlers_disconnect_by_data(buffer,this); g_clear_object(&buffer); }
    if (widget) { g_signal_handlers_disconnect_by_data(widget,this); g_clear_object(&widget); }
    g_clear_object(&native_window);
}
void BtGtkClient::bind(GdkWindow *window) {
    if (destroying) return;
    Keep context_lifetime(context);
    if (native_window != window && bridge().active == this) {
        bridge().scope_known = false; bridge().expire_policy();
    }
    if (window) g_object_ref(window);
    GtkWidget *w = nullptr; if (window) gdk_window_get_user_data(window,reinterpret_cast<gpointer *>(&w));
    bool supported = w && (GTK_IS_ENTRY(w) || GTK_IS_TEXT_VIEW(w));
    if (supported) g_object_ref(w); // keep the new target across unbinding the old one
    clear(); unbind();
    native_window = window; // strong native identity, including field-window recreation
    if (!supported) { metadata(); changed(); return; }
    widget = w; // public Entry/TextView capabilities include ordinary subclasses
    g_signal_connect(w,"notify",G_CALLBACK(on_notify),this); g_signal_connect(w,"button-press-event",G_CALLBACK(on_button),this); g_signal_connect(w,"destroy",G_CALLBACK(on_destroy),this);
    g_signal_connect(w,"hierarchy-changed",G_CALLBACK(on_hierarchy),this);
    if (GTK_IS_ENTRY(w)) g_signal_connect(w,"changed",G_CALLBACK(on_changed),this);
    else { buffer = GTK_TEXT_BUFFER(g_object_ref(gtk_text_view_get_buffer(GTK_TEXT_VIEW(w)))); g_signal_connect(buffer,"changed",G_CALLBACK(on_changed),this); g_signal_connect(buffer,"mark-set",G_CALLBACK(on_mark),this); g_signal_connect(buffer,"apply-tag",G_CALLBACK(+[](GtkTextBuffer *,GtkTextTag *,GtkTextIter *,GtkTextIter *,gpointer d){ static_cast<BtGtkClient *>(d)->changed(); }),this); g_signal_connect(buffer,"remove-tag",G_CALLBACK(+[](GtkTextBuffer *,GtkTextTag *,GtkTextIter *,GtkTextIter *,gpointer d){ static_cast<BtGtkClient *>(d)->changed(); }),this); }
    metadata(); changed();
}
Snapshot BtGtkClient::metadata() {
    Keep context_lifetime(destroying ? nullptr : context);
    Snapshot s; s.revision = revision; s.user = user_revision; s.notification = notification_revision;
    s.focus = focused && bridge().active == this;
    s.field = token(G_OBJECT(context)); // unsupported focused contexts still deny fallback
    auto finish = [&]() {
        if (bridge().active == this && bridge().observe_scope(s)) changed();
        s.revision = revision; s.user = user_revision; s.notification = notification_revision;
        return s;
    };
    if (!widget || gtk_widget_in_destruction(widget)) return finish();
    s.field = token(G_OBJECT(widget));
    s.window = native_window ? token(G_OBJECT(gdk_window_get_toplevel(native_window))) : std::string();
    s.document = token(GTK_IS_ENTRY(widget) ? G_OBJECT(gtk_entry_get_buffer(GTK_ENTRY(widget))) : G_OBJECT(gtk_text_view_get_buffer(GTK_TEXT_VIEW(widget))));
    GtkInputPurpose purpose;
    if (GTK_IS_ENTRY(widget)) {
        auto *entry = GTK_ENTRY(widget);
        purpose = gtk_entry_get_input_purpose(entry);
        s.protected_ = !gtk_entry_get_visibility(entry);
        s.editable = gtk_editable_get_editable(GTK_EDITABLE(entry));
    } else {
        auto *view = GTK_TEXT_VIEW(widget);
        purpose = gtk_text_view_get_input_purpose(view);
        s.protected_ = false;
        s.editable = gtk_text_view_get_editable(view);
        if (buffer != gtk_text_view_get_buffer(view)) {
            s.protected_ = true;
            s.editable = false;
        }
    }
    s.protected_ = s.protected_ || purpose == GTK_INPUT_PURPOSE_PASSWORD || purpose == GTK_INPUT_PURPOSE_PIN;
    s.purpose = s.protected_ ? "password" : purpose == GTK_INPUT_PURPOSE_FREE_FORM ? "free-form" : purpose == GTK_INPUT_PURPOSE_EMAIL ? "email" : purpose == GTK_INPUT_PURPOSE_URL ? "url" : "other";
    s.focus = focused && gtk_widget_has_focus(widget) && gtk_widget_is_sensitive(widget) && gtk_widget_get_mapped(widget) && bridge().active == this;
    if (pending || simple_depth || simple_state == Unknown || !compose || (remote && !remote_ordered)) s.composing = "unknown";
    else if (cached_preedit || remote_preedit || simple_state == Active || xkb_compose_state_get_status(compose) == XKB_COMPOSE_COMPOSING) s.composing = "active";
    else s.composing = "idle";
    return finish();
}
bool BtGtkClient::capture(Snapshot &s,const char *&reason,bool owned_readback) {
    s = metadata();
    if ((bridge().pending_edit || editing) && !(owned_readback && editing)) { reason = "pending"; return false; }
    if (!bridge().enabled()) { reason = "policy"; return false; }
    if (!s.focus || !s.editable || s.protected_) { reason = "privacy"; return false; }
    if (s.composing != "idle") { reason = "composition"; return false; }
    if (GTK_IS_ENTRY(widget)) { auto *e = GTK_ENTRY(widget); if (gtk_entry_buffer_get_length(gtk_entry_get_buffer(e)) > 16000) { reason = "size"; return false; } s.text = gtk_entry_get_text(e); gint c,a; g_object_get(e,"cursor-position",&c,"selection-bound",&a,nullptr); s.caret = c; s.anchor = a; }
    else { GtkTextIter begin,end,c,a; if (gtk_text_buffer_get_char_count(buffer) > 16000) { reason = "size"; return false; } gtk_text_buffer_get_bounds(buffer,&begin,&end);
        // Query tag attributes before content, without allocating per character.
        GtkTextAttributes *attrs = gtk_text_attributes_new();
        for (GtkTextIter it = begin; !gtk_text_iter_is_end(&it); ) {
            gtk_text_iter_get_attributes(&it,attrs);
            if (attrs->invisible) { gtk_text_attributes_unref(attrs); reason = "privacy"; return false; }
            if (!gtk_text_iter_forward_to_tag_toggle(&it,nullptr)) break;
        }
        gtk_text_attributes_unref(attrs);
        for (GtkTextIter it = begin; !gtk_text_iter_is_end(&it); gtk_text_iter_forward_char(&it))
            if (gtk_text_iter_get_child_anchor(&it) || gtk_text_iter_get_pixbuf(&it)) { reason = "content"; return false; }
        gchar *text = gtk_text_buffer_get_text(buffer,&begin,&end,TRUE); s.text = text; g_free(text); gtk_text_buffer_get_iter_at_mark(buffer,&c,gtk_text_buffer_get_insert(buffer)); gtk_text_buffer_get_iter_at_mark(buffer,&a,gtk_text_buffer_get_selection_bound(buffer)); s.caret = gtk_text_iter_get_offset(&c); s.anchor = gtk_text_iter_get_offset(&a);
        if (!gtk_text_iter_can_insert(&c,TRUE)) { reason = "editable"; return false; }
    }
    if (!bounded(s.text)) { reason = "size"; return false; }
    std::string sig = s.field + s.window + s.document + s.purpose + s.composing + sha(s.text) + ":" + std::to_string(s.caret) + ":" + std::to_string(s.anchor);
    if (!editing && !signature.empty() && signature != sig) { ++revision; clear(); }
    signature = sig; s.revision = revision; return true;
}
void BtGtkClient::changed(bool user) {
    if (destroying) return;
    if (editing || clearing) {
        // A real reentrant key remains ordinary input, even while native owned
        // notifications are suppressed. It invalidates an exact readback proof.
        if (user) { ++revision; ++user_revision; ++notification_revision; bridge().deferred_changed = true; }
        return;
    }
    Keep lifetime(context);
    clear(); ++revision; ++notification_revision; if (user) ++user_revision; signature.clear();
    if (!changed_source) changed_source = g_idle_add_full(G_PRIORITY_DEFAULT_IDLE,[](gpointer p)->gboolean {
        auto *c = static_cast<BtGtkClient *>(p); c->changed_source = 0; c->notify(); return G_SOURCE_REMOVE;
    },this,nullptr);
}
void BtGtkClient::notify() {
    if (destroying || bridge().active != this || !bridge().id) return;
    if (bridge().pending_edit) { bridge().deferred_changed = true; return; }
    Keep lifetime(context);
    Snapshot s = metadata(); Dict d; init(d); str(d,"kind","changed");
    str(d,"field_token",s.field); str(d,"window_token",s.window); str(d,"document_token",s.document);
    num(d,"revision",revision); num(d,"user_revision",user_revision); num(d,"notification_revision",notification_revision);
    flag(d,"focus",s.focus); flag(d,"protected",s.protected_); str(d,"composing",s.composing);
    bridge().event(g_variant_builder_end(&d));
}
void BtGtkClient::clear(guint64 cap) {
    if (clearing) return;
    clearing = true;
    if (!cap || inline_offer.capability == cap) inline_offer.clear();
    if (!cap || chooser_offer.capability == cap) chooser_offer.clear();
    clearing = false;
}
void BtGtkPresentation::clear() {
    auto *context = owner.context;
    bool prior = owner.clearing; owner.clearing = true;
    if (clearing) { owner.clearing = prior; return; }
    clearing = true; capability = 0; deadline = 0; ghost_cursor = 0;
    armed = false; armed_word.clear(); armed_all.clear();
    if (expiry_source) { g_source_remove(expiry_source); expiry_source = 0; }
    if (offer) { g_variant_unref(offer); offer = nullptr; }
    values.clear(); buttons.clear(); choice_cursor = 0;
    if (popover) { GtkWidget *p = popover; popover = nullptr; gtk_widget_destroy(p); }
    if (!ghost.empty()) { ghost.clear(); g_signal_emit_by_name(context,"preedit-changed"); g_signal_emit_by_name(context,"preedit-end"); }
    clearing = false;
    owner.clearing = prior;
}
bool BtGtkClient::witness(GVariant *o,Snapshot &s,const char *&reason) {
    if (!complete_offer(o)) { reason = "offer"; return false; }
    if (!capture(s,reason)) return false;
    if (!o || nget(o,"policy_epoch") != nget(bridge().policy,"policy_epoch") || nget(o,"expires_ns") <= now()) { reason = "lease"; return false; }
    if (sget(o,"field_token") != s.field || sget(o,"window_token") != s.window || sget(o,"document_token") != s.document || nget(o,"revision") != s.revision || nget(o,"user_revision") != s.user || sget(o,"text_sha256") != sha(s.text) || uget(o,"caret") != s.caret || uget(o,"anchor") != s.anchor || sget(o,"offset_unit") != "scalar") { reason = "witness"; return false; }
    guint size = g_utf8_strlen(s.text.c_str(),-1), start = uget(o,"start"), length = uget(o,"length");
    if (start > size || length > size - start) { reason = "range"; return false; } return true;
}
bool BtGtkPresentation::ghost_visible() {
    auto *widget = owner.widget;
    Snapshot state = owner.metadata();
    if (ghost.empty() || !widget || !bridge().enabled() ||
        !state.focus || state.protected_ || !state.editable || state.composing != "idle") return false;
    if (GTK_IS_TEXT_VIEW(widget)) return textview_ghost_visible();
    const char *committed = gtk_entry_get_text(GTK_ENTRY(widget));
    int begin = int(g_utf8_offset_to_pointer(committed,gtk_editable_get_position(GTK_EDITABLE(widget)))-committed);
    std::string expected(committed,begin); expected += ghost; expected += committed+begin;
    PangoLayout *layout = gtk_entry_get_layout(GTK_ENTRY(widget));
    const char *text = pango_layout_get_text(layout);
    if (expected != text) return false; // actual native layout, not substring coincidence
    int bytes = int(ghost.size());
    int x,y; gtk_entry_get_layout_offsets(GTK_ENTRY(widget),&x,&y); GdkRectangle area; gtk_entry_get_text_area(GTK_ENTRY(widget),&area);
    int ax = area.x, ay = area.y, width = area.width, height = area.height;
    for (int i = begin; i < begin+bytes; i = int(g_utf8_next_char(text+i)-text)) { PangoRectangle r; pango_layout_index_to_pos(layout,i,&r); int left = x+PANGO_PIXELS(std::min(r.x,r.x+r.width))-ax, right = x+PANGO_PIXELS(std::max(r.x,r.x+r.width))-ax; int top = y+PANGO_PIXELS(r.y)-ay; if (left < 0 || right > width || top < 0 || top+PANGO_PIXELS_CEIL(r.height) > height) return false; }
    return true;
}
bool BtGtkPresentation::textview_ghost_visible() {
    auto *widget = owner.widget; auto *context = owner.context;
    Keep target(widget), im(context);
    // Public GTK3 get_cursor_locations includes the actual IM preedit cursor
    // offset (gtktextview.c:2070-2130). Probe native start/end and restore cursor
    // zero in the same GUI callback. No glyph estimate or pixel acknowledgment.
    if (gtk_widget_get_direction(widget) != GTK_TEXT_DIR_LTR) return false;
    for (const char *p = ghost.c_str(); *p; p = g_utf8_next_char(p)) {
        gunichar ch = g_utf8_get_char(p);
        if (pango_unichar_direction(ch) == PANGO_DIRECTION_RTL ||
            g_unichar_type(ch) == G_UNICODE_FORMAT || g_unichar_iscntrl(ch)) return false;
    }
    auto *view = GTK_TEXT_VIEW(widget);
    GdkRectangle start,weak_start,end,weak_end,visible;
    gtk_text_view_get_cursor_locations(view,nullptr,&start,&weak_start);
    std::string shown = ghost;
    ghost_cursor = g_utf8_strlen(ghost.c_str(),-1);
    g_signal_emit_by_name(context,"preedit-changed");
    if (ghost != shown) { ghost_cursor = 0; return false; }
    gtk_text_view_get_cursor_locations(view,nullptr,&end,&weak_end);
    ghost_cursor = 0; g_signal_emit_by_name(context,"preedit-changed");
    if (ghost != shown) return false;
    gtk_text_view_get_cursor_locations(view,nullptr,&start,&weak_start);
    gtk_text_view_get_visible_rect(view,&visible);
    return start.x == weak_start.x && end.x == weak_end.x &&
        start.y == end.y && start.height == end.height && end.x > start.x &&
        start.x >= visible.x && end.x <= visible.x+visible.width &&
        start.y >= visible.y && start.y+start.height <= visible.y+visible.height;
}
bool BtGtkPresentation::choice_visible(guint index) {
    if (!popover || !gtk_widget_get_mapped(popover) || index >= values.size() || index >= buttons.size()) return false;
    GtkWidget *button = buttons[index], *box = gtk_widget_get_parent(button);
    if (!box || !gtk_widget_get_mapped(button)) return false;
    GtkRequisition min,natural; GtkAllocation a,outer;
    gtk_widget_get_preferred_size(button,&min,&natural);
    gtk_widget_get_allocation(button,&a); gtk_widget_get_allocation(box,&outer);
    // GtkBox and its windowless children share the parent's allocation basis.
    return a.width >= natural.width && a.height >= natural.height &&
        a.x >= outer.x && a.y >= outer.y &&
        a.x+a.width <= outer.x+outer.width && a.y+a.height <= outer.y+outer.height;
}
void BtGtkPresentation::choose(guint index) {
    if (index >= values.size()) return;
    for (guint i = 0; i < values.size(); ++i) {
        if (i == index) gtk_widget_set_state_flags(buttons[i],GTK_STATE_FLAG_SELECTED,FALSE);
        else gtk_widget_unset_state_flags(buttons[i],GTK_STATE_FLAG_SELECTED);
    }
    choice_cursor = index;
}
GVariant *BtGtkClient::present(GVariant *o) {
    const std::string mode = sget(o,"mode");
    if (mode == "completion" || mode == "correction") return inline_offer.present(o);
    if (mode == "emoji" || mode == "synonyms" || mode == "alternatives") return chooser_offer.present(o);
    return refusal("feature");
}
bool BtGtkClient::arm(GVariant *authorization) {
    guint64 cap = nget(authorization,"capability_id");
    if (cap && cap == inline_offer.capability) return inline_offer.arm(authorization);
    if (cap && cap == chooser_offer.capability) return chooser_offer.arm(authorization);
    return false;
}
GVariant *BtGtkPresentation::present(GVariant *o) {
    auto *widget = owner.widget; auto *context = owner.context;
    clear(); Snapshot s; const char *reason = "offer"; if (!owner.witness(o,s,reason)) return refusal(reason);
    std::string mode = sget(o,"mode"), text = sget(o,"text"); bool preedit = mode == "completion" || mode == "correction";
    const char *feature = mode == "completion" ? "predictions" : mode == "correction" ? "correction" : mode == "emoji" ? "emoji" : mode == "synonyms" ? "synonyms" : mode == "alternatives" ? "alternatives" : nullptr;
    if (!feature || !bget(bridge().policy,feature)) return refusal("feature");
    if (preedit) {
        bool overwrite = GTK_IS_ENTRY(widget) ? gtk_entry_get_overwrite_mode(GTK_ENTRY(widget)) : gtk_text_view_get_overwrite(GTK_TEXT_VIEW(widget));
        if (!owner.use_preedit || s.caret != s.anchor || !bounded(text) || text.empty() ||
            text.find('\n') != std::string::npos || text.find('\r') != std::string::npos || overwrite)
            return refusal("preedit");
        ghost = text; g_signal_emit_by_name(context,"preedit-start"); g_signal_emit_by_name(context,"preedit-changed");
        if (!ghost_visible()) { clear(); return refusal("layout"); }
    } else {
        GVariant *v = g_variant_lookup_value(o,"values",G_VARIANT_TYPE_STRING_ARRAY); if (!v) return refusal("values");
        popover = gtk_popover_new(widget); g_object_set(popover,"modal",FALSE,nullptr); GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL,0); gtk_container_add(GTK_CONTAINER(popover),box);
        GVariantIter it; g_variant_iter_init(&it,v); const char *value; guint index = 0;
        while (index < 8 && g_variant_iter_next(&it,"&s",&value)) { std::string val(value); if (val.empty() || !bounded(val) || val.find('\n') != std::string::npos || val.find('\r') != std::string::npos) break;
            GtkWidget *button = gtk_button_new_with_label(value); gtk_widget_set_can_focus(button,FALSE); g_object_set_data(G_OBJECT(button),"brotypist-index",GUINT_TO_POINTER(index));
            g_signal_connect(button,"clicked",G_CALLBACK(+[](GtkButton *b,gpointer data) { static_cast<BtGtkPresentation *>(data)->accept("choice",GPOINTER_TO_UINT(g_object_get_data(G_OBJECT(b),"brotypist-index"))); }),this);
            gtk_box_pack_start(GTK_BOX(box),button,FALSE,FALSE,0); values.push_back(val); buttons.push_back(button); ++index;
        } g_variant_unref(v);
        if (values.empty()) { clear(); return refusal("values"); }
        gtk_widget_show_all(popover); gtk_container_check_resize(GTK_CONTAINER(popover));
        if (!gtk_widget_get_mapped(popover)) { clear(); return refusal("layout"); }
        guint visible = 0;
        while (visible < values.size() && choice_visible(visible)) ++visible;
        for (guint i = visible; i < buttons.size(); ++i) gtk_widget_hide(buttons[i]);
        values.resize(visible); buttons.resize(visible);
        if (values.empty()) { clear(); return refusal("layout"); }
        choose(0);
    }
    // Signals/layout can reenter the host. Revalidate after native delivery.
    if (!owner.witness(o,s,reason)) { clear(); return refusal(reason); }
    offer = g_variant_ref(o); capability = ++bridge().serial; deadline = std::min({nget(o,"expires_ns"),nget(bridge().policy,"expires_ns"),now()+lease});
    expiry_source = g_timeout_add(guint(std::max<guint64>(1,(deadline-now()+999999)/1000000)),[](gpointer p)->gboolean {
        auto *offer = static_cast<BtGtkPresentation *>(p);
        Keep lifetime(offer->owner.context); offer->expiry_source = 0; offer->clear(); return G_SOURCE_REMOVE;
    },this);
    Dict d; init(d); flag(d,"presented",true); num(d,"capability_id",capability); num(d,"offer_id",nget(o,"offer_id")); num(d,"expires_ns",deadline); str(d,"shown_text",ghost); Dict array; g_variant_builder_init(&array,G_VARIANT_TYPE_STRING_ARRAY); for (const auto &v : values) g_variant_builder_add(&array,"s",v.c_str()); g_variant_builder_add(&d,"{sv}","shown_values",g_variant_builder_end(&array)); return g_variant_builder_end(&d);
}
bool BtGtkPresentation::arm(GVariant *authorization) {
    if (!offer || !capability || now() >= deadline) return false;
    const guint64 live_capability = capability;
    for (const char *k : {"offer_id","capability_id","policy_epoch","expires_ns"})
        if (!typed(authorization,k,G_VARIANT_TYPE_UINT64)) return false;
    for (const char *k : {"word_text","all_text","shown_text"})
        if (!typed(authorization,k,G_VARIANT_TYPE_STRING)) return false;
    if (!typed(authorization,"shown_values",G_VARIANT_TYPE_STRING_ARRAY) ||
        nget(authorization,"offer_id") != nget(offer,"offer_id") ||
        nget(authorization,"capability_id") != capability ||
        nget(authorization,"policy_epoch") != nget(offer,"policy_epoch") ||
        nget(authorization,"expires_ns") != deadline ||
        sget(authorization,"shown_text") != ghost) return false;
    GVariant *shown = g_variant_lookup_value(authorization,"shown_values",G_VARIANT_TYPE_STRING_ARRAY);
    bool same = g_variant_n_children(shown) == values.size();
    GVariantIter it; const char *value; guint index = 0; g_variant_iter_init(&it,shown);
    while (g_variant_iter_next(&it,"&s",&value)) {
        if (index >= values.size() || values[index] != value) same = false;
        ++index;
    }
    g_variant_unref(shown);
    if (!same) return false;
    GVariant *o = g_variant_ref(offer); Snapshot s; const char *reason = "witness";
    bool fresh = owner.witness(o,s,reason); g_variant_unref(o);
    if (!fresh || (!ghost.empty() && !ghost_visible())) { clear(); return false; }
    for (guint i = 0; i < values.size(); ++i) if (!choice_visible(i)) { clear(); return false; }
    if (!offer || capability != live_capability || now() >= deadline) return false;
    std::string word = sget(authorization,"word_text"), all = sget(authorization,"all_text");
    if (!bounded(word) || !bounded(all)) return false;
    // The Core computes word boundaries/punctuation/trailing space after seeing
    // the actual receipt. Do not recompute Suggestion::accept in this SDK.
    if (!ghost.empty() && all.empty()) return false;
    armed_word = std::move(word); armed_all = std::move(all); armed = true;
    return true;
}
void BtGtkClient::select(guint caret,guint anchor) { if (GTK_IS_ENTRY(widget)) gtk_editable_select_region(GTK_EDITABLE(widget),anchor,caret); else { GtkTextIter c,a; gtk_text_buffer_get_iter_at_offset(buffer,&c,caret); gtk_text_buffer_get_iter_at_offset(buffer,&a,anchor); gtk_text_buffer_select_range(buffer,&c,&a); } }
bool BtGtkPresentation::accept(const char *action,guint index) {
    auto *widget = owner.widget; auto *context = owner.context; auto *buffer = owner.buffer;
    Keep target_lifetime(widget), context_lifetime(context), buffer_lifetime(buffer);
    auto &editing = owner.editing; auto &revision = owner.revision;
    auto &notification_revision = owner.notification_revision; auto &signature = owner.signature;
    if (!capability || !offer || now() >= deadline) { clear(); return false; }
    if (!armed) return false;
    if (bridge().pending_edit) return false;
    const guint64 accepting_capability = capability;
    GVariant *o = g_variant_ref(offer); Snapshot before; const char *reason = "witness";
    if (!owner.witness(o,before,reason) ||
        !(!ghost.empty() ? ghost_visible() : choice_visible(index))) {
        g_variant_unref(o); clear(); return false;
    }
    if (!offer || !armed || capability != accepting_capability || now() >= deadline ||
        owner.widget != widget || owner.buffer != buffer) { g_variant_unref(o); clear(); return false; }
    std::string accepted;
    if (!std::strcmp(action,"choice")) { if (index >= values.size()) { g_variant_unref(o); return false; } accepted = values[index]; }
    else if (!std::strcmp(action,"word")) accepted = armed_word;
    else accepted = armed_all;
    if (accepted.empty() || !bounded(accepted)) { g_variant_unref(o); return false; }
    guint start = uget(o,"start"), length = uget(o,"length"); guint end = start+length;
    if (sget(o,"mode") == "synonyms" && (start != std::min(before.caret,before.anchor) || end != std::max(before.caret,before.anchor) || !length)) { g_variant_unref(o); clear(); return false; }
    if (GTK_IS_ENTRY(widget)) { auto *e = GTK_ENTRY(widget); guint max = gtk_entry_get_max_length(e); guint expected = g_utf8_strlen(before.text.c_str(),-1)-length+g_utf8_strlen(accepted.c_str(),-1); if (gtk_entry_get_overwrite_mode(e) || accepted.find('\n') != std::string::npos || accepted.find('\r') != std::string::npos || (max && expected > max)) { g_variant_unref(o); clear(); return false; } }
    else {
        if (!length && gtk_text_view_get_overwrite(GTK_TEXT_VIEW(widget))) { g_variant_unref(o); clear(); return false; }
        GtkTextIter it;
        for (guint pos = start; pos <= end; ++pos) {
            gtk_text_buffer_get_iter_at_offset(buffer,&it,pos);
            if (!gtk_text_iter_editable(&it,TRUE)) { g_variant_unref(o); clear(); return false; }
        }
    }
    guint64 cap = capability, edit_deadline = deadline;
    GtkWidget *target = GTK_WIDGET(g_object_ref(widget));
    GtkIMContext *im = GTK_IM_CONTEXT(g_object_ref(context));
    GtkTextBuffer *transaction_buffer = buffer ? GTK_TEXT_BUFFER(g_object_ref(buffer)) : nullptr;
    bool selected_range = std::min(before.caret,before.anchor) == start && std::max(before.caret,before.anchor) == end;
    guint stage_caret = selected_range ? before.caret : end;
    guint stage_anchor = selected_range ? before.anchor : start;
    editing = true; owner.clear();
    if (transaction_buffer) gtk_text_buffer_begin_user_action(transaction_buffer);
    if (owner.widget == target && owner.buffer == buffer) owner.select(stage_caret,stage_anchor);
    Snapshot staged;
    bool valid = owner.widget == target && owner.buffer == buffer && owner.capture(staged,reason,true) && now() < edit_deadline &&
        nget(o,"policy_epoch") == nget(bridge().policy,"policy_epoch") &&
        staged.field == before.field && staged.window == before.window &&
        staged.document == before.document && staged.text == before.text &&
        staged.caret == stage_caret && staged.anchor == stage_anchor && revision == before.revision;
    if (!valid) {
        // Restore only the exact still-owned selection, never after dispatch.
        if (staged.field == before.field && staged.text == before.text &&
            staged.caret == stage_caret && staged.anchor == stage_anchor)
            owner.select(before.caret,before.anchor);
        if (transaction_buffer) { gtk_text_buffer_end_user_action(transaction_buffer); g_object_unref(transaction_buffer); }
        editing = false; g_object_unref(target); g_object_unref(im); g_variant_unref(o); owner.changed(); return false;
    }
    Dict frozen; init(frozen); str(frozen,"kind","accepted");
    num(frozen,"acceptance_id",++bridge().acceptance); num(frozen,"offer_id",nget(o,"offer_id"));
    num(frozen,"capability_id",cap); str(frozen,"action",action);
    if (!std::strcmp(action,"choice")) u32(frozen,"choice_index",index);
    u32(frozen,"start",start); u32(frozen,"length",length); str(frozen,"text",accepted);
    g_variant_builder_add(&frozen,"{sv}","before",before.dict(true));
    const guint64 dispatch_ns = now(); num(frozen,"accepted_ns",dispatch_ns);
    bridge().begin_edit(g_variant_builder_end(&frozen),dispatch_ns);
    g_signal_emit_by_name(im,"commit",accepted.c_str()); // original native dispatch timestamp is fixed above
    if (transaction_buffer) { gtk_text_buffer_end_user_action(transaction_buffer); g_object_unref(transaction_buffer); }
    signature.clear(); Snapshot after; bool readback = owner.capture(after,reason,true);
    std::string expected = slice(before.text,0,start)+accepted+slice(before.text,end,g_utf8_strlen(before.text.c_str(),-1)-end);
    guint expected_caret = start+g_utf8_strlen(accepted.c_str(),-1);
    bool same_user = readback && after.user == before.user;
    const char *outcome = same_user && after.field == before.field && after.document == before.document &&
        after.text == expected && after.caret == expected_caret && after.anchor == expected_caret ? "applied" :
        same_user && after.field == before.field && after.document == before.document &&
        after.text == before.text ? "rejected" : "indeterminate";
    ++revision; ++notification_revision; after.revision = revision; after.notification = notification_revision;
    GVariantDict terminal; g_variant_dict_init(&terminal,bridge().pending_receipt);
    g_variant_dict_insert_value(&terminal,"outcome",g_variant_new_string(outcome));
    g_variant_dict_insert_value(&terminal,"after",after.dict(readback));
    bridge().finish_edit(g_variant_dict_end(&terminal));
    editing = false; owner.notify(); g_object_unref(target); g_object_unref(im); g_variant_unref(o); return true; // dispatch uncertainty consumes once, never retries
}

BtGtkClient *bt_gtk_new(GtkIMContext *context,GtkIMContext *simple) { const char *bus = g_getenv("BROTYPIST_NATIVE_BUS"); if (!bus || !*bus || !g_dbus_is_name(bus) || !GTK_IS_IM_CONTEXT_SIMPLE(simple)) return nullptr; auto *c = new BtGtkClient(context); bridge().start(); return c; }
void bt_gtk_free(BtGtkClient *c) { delete c; }
void bt_gtk_window(BtGtkClient *c,GdkWindow *w) { if(c) c->bind(w); }
void bt_gtk_focus(BtGtkClient *c,bool focus) {
    if (!c) return;
    auto &b = bridge();
    if (focus && b.active != c) {
        b.expire_policy();
        if (b.active) b.active->changed();
        b.active = c;
    }
    c->focused = focus; c->metadata(); c->changed();
}
void bt_gtk_revoke(BtGtkClient *c) { if(c) c->changed(); }
void bt_gtk_state(BtGtkClient *c,xkb_compose_state *compose,bool preedit,bool remote,bool use,
                  bool ordered,bool cached) {
    if (!c) return;
    preedit = remote && preedit;
    bool change = c->compose != compose || c->remote_preedit != preedit || c->remote != remote ||
        c->use_preedit != use || c->remote_ordered != ordered || c->cached_preedit != cached;
    c->compose = compose; c->remote_preedit = preedit; c->remote = remote; c->use_preedit = use;
    c->remote_ordered = ordered; c->cached_preedit = cached;
    if (change) c->changed(); // never reset local xkb/Simple or retained user preedit here
}
void bt_gtk_pending(BtGtkClient *c,int delta) { if (!c) return; if (delta > 0) c->pending += delta; else c->pending -= std::min(c->pending,unsigned(-delta)); c->changed(); }
void bt_gtk_simple_begin(BtGtkClient *c,GdkEventKey *event) {
    if (!c) return;
    ++c->simple_depth;
    c->simple_resolved = c->simple_committed = c->simple_started = false;
    c->simple_empty_before = c->simple_state == BtGtkClient::Idle;
    // Source-bound proof: GTK 3.24.52 no_sequence_matches, n_compose == 1,
    // clears compose_buffer[0] before the ordinary printable commit. A custom
    // compose-table match instead emits preedit-start, invalidating this proof.
    // Ctrl/Alt/Super and nonprintable/dead/Compose/hex starters are never guessed.
    gunichar ch = gdk_keyval_to_unicode(event->keyval);
    c->simple_ordinary = gtk_get_major_version() == 3 && gtk_get_minor_version() == 24 &&
        gtk_get_micro_version() == 52 && event->type == GDK_KEY_PRESS &&
        ch && !g_unichar_iscntrl(ch) &&
        !(event->state & (GDK_CONTROL_MASK|GDK_MOD1_MASK|GDK_SUPER_MASK|GDK_MOD4_MASK));
}
void bt_gtk_simple_commit(BtGtkClient *c) {
    // Boolean lifecycle only. Never inspect or retain a user commit's text.
    if (c && c->simple_depth) c->simple_committed = true;
    bt_gtk_revoke(c);
}
void bt_gtk_simple_end(BtGtkClient *c,bool consumed) {
    if (!c) return;
    if (c->simple_depth) --c->simple_depth;
    if (consumed && !c->simple_resolved)
        c->simple_state = c->simple_empty_before && c->simple_ordinary &&
            c->simple_committed && !c->simple_started ? BtGtkClient::Idle : BtGtkClient::Unknown;
    c->changed();
}
void bt_gtk_simple_idle(BtGtkClient *c) { if(c) { c->simple_resolved = true; c->simple_state = BtGtkClient::Idle; c->changed(); } }
void bt_gtk_simple_active(BtGtkClient *c) { if(c) { c->simple_resolved = true; c->simple_started = true; c->simple_state = BtGtkClient::Active; c->changed(); } }
void bt_gtk_reset_complete(BtGtkClient *c) { bt_gtk_simple_idle(c); }
bool bt_gtk_preedit(BtGtkClient *c,gchar **text,PangoAttrList **attrs,gint *cursor) {
    if (!c || c->inline_offer.ghost.empty()) return false;
    const auto &p = c->inline_offer;
    if (text) *text = g_strdup(p.ghost.c_str());
    if (cursor) *cursor = p.ghost_cursor;
    if (attrs) {
        *attrs = pango_attr_list_new();
        PangoAttribute *a = pango_attr_foreground_new(0x8888,0x8888,0x8888);
        a->start_index = 0; a->end_index = p.ghost.size(); pango_attr_list_insert(*attrs,a);
        a = pango_attr_underline_new(PANGO_UNDERLINE_SINGLE);
        a->start_index = 0; a->end_index = p.ghost.size(); pango_attr_list_insert(*attrs,a);
    }
    // DontCommit by construction: never stored in Fcitx commit_preedit_string.
    return true;
}
namespace {
guint modifiers(GdkEventKey *event) {
    GdkModifierType state = static_cast<GdkModifierType>(event->state);
    if (event->window)
        gdk_keymap_add_virtual_modifiers(gdk_keymap_get_for_display(gdk_window_get_display(event->window)),&state);
    return state;
}
bool shortcut_matches(const char *value, GdkEventKey *e,guint state) {
    const char *key = value;
    guint required = 0;
    while (const char *plus = std::strchr(key,'+')) {
        size_t length = plus-key;
        if (length == 4 && !std::memcmp(key,"Ctrl",4)) required |= GDK_CONTROL_MASK;
        else if (length == 3 && !std::memcmp(key,"Alt",3)) required |= GDK_MOD1_MASK;
        else if (length == 5 && !std::memcmp(key,"Shift",5)) required |= GDK_SHIFT_MASK;
        else if (length == 5 && !std::memcmp(key,"Super",5)) required |= GDK_SUPER_MASK;
        else return false;
        key = plus+1;
    }
    if (e->state & GDK_MOD5_MASK) return false; // AltGr is never Ctrl+Alt.
    if (e->window && (e->state & GDK_CONTROL_MASK) && (e->state & GDK_MOD1_MASK)) {
        guint translated; gint group, level; GdkModifierType consumed;
        GdkKeymap *map = gdk_keymap_get_for_display(gdk_window_get_display(e->window));
        if (gdk_keymap_translate_keyboard_state(map,e->hardware_keycode,static_cast<GdkModifierType>(e->state),e->group,&translated,&group,&level,&consumed) &&
            (consumed & (GDK_CONTROL_MASK|GDK_MOD1_MASK))) return false;
    }
    guint mask = GDK_CONTROL_MASK|GDK_MOD1_MASK|GDK_SHIFT_MASK|GDK_SUPER_MASK;
    if ((state & mask) != required || !*key) return false;
    if (!std::strcmp(key,"AboveTab")) return e->hardware_keycode == 49;
    guint keyval = !std::strcmp(key,"Space") ? GDK_KEY_space : gdk_keyval_from_name(key);
    guint actual = e->keyval == GDK_KEY_ISO_Left_Tab ? GDK_KEY_Tab : e->keyval;
    return keyval && keyval != GDK_KEY_VoidSymbol && gdk_keyval_to_lower(actual) == gdk_keyval_to_lower(keyval);
}
}
bool bt_gtk_key(BtGtkClient *c,GdkEventKey *e) {
    if (!c || e->type != GDK_KEY_PRESS) return false;
    const Snapshot key_scope = c->metadata(); // freeze the actual key-handling origin, without text
    guint state = modifiers(e);
    auto &b = bridge(); GVariant *shortcuts = b.policy ? g_variant_lookup_value(b.policy,"shortcuts",G_VARIANT_TYPE("a{ss}")) : nullptr;
    const guint64 key_generation = b.focus_generation, key_epoch = nget(b.policy,"policy_epoch");
    std::string action;
    if (shortcuts && b.live()) {
        GVariantIter it; const char *a,*value; g_variant_iter_init(&it,shortcuts);
        while (g_variant_iter_next(&it,"{&s&s}",&a,&value)) if (shortcut_matches(value,e,state)) { action = a; break; }
    }
    if (shortcuts) g_variant_unref(shortcuts);
    auto &ghost = c->inline_offer; auto &choices = c->chooser_offer;
    if (action == "word" || action == "all") {
        if (ghost.capability && ghost.offer && now() < ghost.deadline) {
            if (ghost.accept(action.c_str())) return true;
        } else if (choices.offer && (sget(choices.offer,"mode") == "emoji" || sget(choices.offer,"mode") == "synonyms")) {
            if (choices.accept("choice",0)) return true;
        }
    }
    if (action.empty() && choices.popover && choices.armed && choices.capability && choices.offer && now() < choices.deadline &&
        !(state & (GDK_SHIFT_MASK|GDK_CONTROL_MASK|GDK_MOD1_MASK|GDK_SUPER_MASK|GDK_MOD4_MASK|GDK_MOD5_MASK)) &&
        (e->keyval == GDK_KEY_Up || e->keyval == GDK_KEY_Down || e->keyval == GDK_KEY_Return || e->keyval == GDK_KEY_KP_Enter)) {
        Snapshot s; const char *reason = "witness"; GVariant *o = g_variant_ref(choices.offer);
        bool valid = c->witness(o,s,reason); g_variant_unref(o);
        if (valid && !choices.values.empty()) {
            if (e->keyval == GDK_KEY_Return || e->keyval == GDK_KEY_KP_Enter) {
                if (choices.accept("choice",choices.choice_cursor)) return true;
            } else {
                guint index = e->keyval == GDK_KEY_Down ? (choices.choice_cursor+1)%choices.values.size() :
                    (choices.choice_cursor+choices.values.size()-1)%choices.values.size();
                if (choices.choice_visible(index)) { choices.choose(index); return true; }
            }
        }
    }
    if (action == "toggle" || action == "app_toggle") {
        c->clear();
        if (b.action(action.c_str(),key_scope,key_generation,key_epoch)) return true; // metadata-only
    }
    if (action == "force" || action == "alternatives" || action == "synonyms") {
        Snapshot s; const char *reason = "target";
        if (c->capture(s,reason)) {
            c->clear();
            if (b.action(action.c_str(),key_scope,key_generation,key_epoch)) return true;
        }
    }
    BtGtkPresentation &dismiss = choices.capability && now() < choices.deadline ? choices : ghost;
    if (e->keyval == GDK_KEY_Escape && dismiss.capability && dismiss.offer && now() < dismiss.deadline) {
        Snapshot before; const char *reason = "witness"; GVariant *o = g_variant_ref(dismiss.offer);
        const guint64 cap = dismiss.capability;
        bool valid = c->witness(o,before,reason);
        std::string behavior = sget(b.policy,"escape_behavior");
        if (valid && (behavior == "pause" || behavior == "app" || behavior == "passthrough")) {
            Dict d; init(d); str(d,"kind","dismissed"); num(d,"offer_id",nget(o,"offer_id")); num(d,"capability_id",cap);
            g_variant_builder_add(&d,"{sv}","before",before.dict(true)); num(d,"accepted_ns",now()); flag(d,"bypass",false);
            c->clear(); b.event(g_variant_builder_end(&d)); g_variant_unref(o);
            c->changed(true); return behavior != "passthrough";
        }
        g_variant_unref(o);
    }
    c->changed(true); return false;
}
