// SPDX-License-Identifier: BSD-3-Clause
#include "brotypistqtclient.h"

#include <QBackingStore>
#include <QCoreApplication>
#include <QCryptographicHash>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDBusArgument>
#include <QFontMetricsF>
#include <QGlyphRun>
#include <QGuiApplication>
#include <QInputMethod>
#include <QInputMethodEvent>
#include <QKeyEvent>
#include <QLineEdit>
#include <QMouseEvent>
#include <QPalette>
#include <QtMath>
#include <QPainter>
#include <QPlainTextEdit>
#include <QRawFont>
#include <QRandomGenerator>
#include <QScreen>
#include <QTextBlock>
#include <QTextCharFormat>
#include <QTextEdit>
#include <QTextLayout>
#include <QStyle>
#include <QStyleOptionFrame>
#include <QScopeGuard>
#include <QThread>
#include <QTimer>
#include <QWidget>
#include <algorithm>
#include <ctime>
#include <limits>
#include <memory>
#include <vector>
#include <utility>

namespace brotypist {
namespace {
qulonglong nowNs() {
    timespec time{};
    if (clock_gettime(CLOCK_MONOTONIC, &time) != 0) return 0;
    return qulonglong(time.tv_sec) * 1000000000ULL + qulonglong(time.tv_nsec);
}
QVariant u64(qulonglong value) { return QVariant::fromValue(value); }
bool boundary(const QString &text, int offset) {
    return offset >= 0 && offset <= text.size() &&
        !(offset > 0 && offset < text.size() && text[offset - 1].isHighSurrogate() && text[offset].isLowSurrogate());
}
bool bounded(const QString &text) {
    int scalars = 0;
    for (qsizetype i = 0; i < text.size(); ++i) {
        if (++scalars > 16000 || text[i].isLowSurrogate()) return false;
        if (text[i].isHighSurrogate() && (++i >= text.size() || !text[i].isLowSurrogate())) return false;
    }
    return text.toUtf8().size() <= 65536;
}
QString hash(const QString &text) {
    return QString::fromLatin1(QCryptographicHash::hash(text.toUtf8(), QCryptographicHash::Sha256).toHex());
}
QVariantMap mapValue(const QVariant &value) {
    if (value.metaType() == QMetaType::fromType<QDBusArgument>()) return qdbus_cast<QVariantMap>(value);
    return value.toMap();
}
QStringList stringList(const QVariant &value) {
    if (value.metaType() == QMetaType::fromType<QDBusArgument>()) return qdbus_cast<QStringList>(value);
    return value.toStringList();
}
QVariantMap refusal(const char *reason) {
    return {{QStringLiteral("presented"), false}, {QStringLiteral("reason"), QString::fromLatin1(reason)}};
}
QString nativeKey(const QKeyEvent &event) {
    // QFcitx receives the toolkit's XKB native scan code unchanged.
    const auto scan = event.nativeScanCode();
    QString key;
    const bool xkbBackend = QGuiApplication::platformName() == QStringLiteral("xcb") ||
        QGuiApplication::platformName().startsWith(QStringLiteral("wayland"));
    if (xkbBackend && scan == 49) key = QStringLiteral("AboveTab");
    else if (event.key() == Qt::Key_Tab || event.key() == Qt::Key_Backtab) key = QStringLiteral("Tab");
    else if (event.key() == Qt::Key_Escape) key = QStringLiteral("Escape");
    else if (event.key() == Qt::Key_Space) key = QStringLiteral("Space");
    else if (event.key() == Qt::Key_Return || event.key() == Qt::Key_Enter) key = QStringLiteral("Return");
    else if ((event.key() >= Qt::Key_A && event.key() <= Qt::Key_Z) ||
             (event.key() >= Qt::Key_0 && event.key() <= Qt::Key_9)) key = QChar(event.key());
    else if (event.key() >= Qt::Key_F1 && event.key() <= Qt::Key_F12) key = QStringLiteral("F%1").arg(event.key() - Qt::Key_F1 + 1);
    if (key.isEmpty() || event.modifiers().testFlag(Qt::GroupSwitchModifier) ||
        (event.nativeModifiers() & (1U << 7))) return {};
    QString modifiers;
    if (event.modifiers().testFlag(Qt::ControlModifier)) modifiers += QStringLiteral("Ctrl+");
    if (event.modifiers().testFlag(Qt::AltModifier)) modifiers += QStringLiteral("Alt+");
    if (event.modifiers().testFlag(Qt::ShiftModifier)) modifiers += QStringLiteral("Shift+");
    if (event.modifiers().testFlag(Qt::MetaModifier)) modifiers += QStringLiteral("Super+");
    return modifiers + key;
}
bool keyMatches(const QKeyEvent &event, const QString &sequence) {
    return !sequence.isEmpty() && nativeKey(event) == sequence;
}
} // namespace

// A native non-focus-taking transient window works with QGuiApplication as well
// as QApplication. Every returned choice is measured, painted without elision,
// and fits entirely inside one available screen rectangle. No pixel claim.
class ChoiceWindow final : public QWindow {
public:
    ChoiceWindow(QtClient *client, QWindow *parent, const QStringList &values,
                 const QFont &font, const QPoint &anchor)
        : client_(client), store_(this), values_(values), font_(font) {
        setFlags(Qt::ToolTip | Qt::FramelessWindowHint | Qt::WindowDoesNotAcceptFocus);
        setTransientParent(parent);
        setScreen(parent->screen());
        const QRect available = screen()->availableGeometry();
        int width = 0;
        const int limit = std::min(9, int(values_.size()));
        for (int i = 0; i < limit; ++i) {
            if (values_[i].isEmpty() || values_[i].contains(QChar::LineFeed) ||
                values_[i].contains(QChar::CarriageReturn) || values_[i].contains(QChar::Tabulation) ||
                values_[i].contains(QChar::LineSeparator) || values_[i].contains(QChar::ParagraphSeparator)) break;
            auto layout = std::make_unique<QTextLayout>(QString::number(i + 1) + QStringLiteral("  ") + values_[i], font_);
            QTextOption option; option.setWrapMode(QTextOption::NoWrap); option.setTextDirection(Qt::LeftToRight);
            layout->setTextOption(option); layout->beginLayout();
            auto line = layout->createLine();
            if (line.isValid()) line.setLineWidth(16000000);
            layout->endLayout();
            if (!line.isValid()) break;
            QRectF occupied(0, 0, line.naturalTextWidth(), line.height());
            for (const auto &run : line.glyphRuns()) occupied = occupied.united(run.boundingRect());
            const int candidateWidth = qCeil(occupied.width()) + 24;
            const int candidateRow = std::max(row_, qCeil(occupied.height()) + 12);
            if (candidateWidth > available.width() || (i + 1) * candidateRow > available.height()) break;
            width = std::max(width, candidateWidth); row_ = candidateRow;
            bounds_.push_back(occupied); layouts_.push_back(std::move(layout)); ++count_;
        }
        if (!count_) return;
        const QSize size(width, row_ * count_);
        const QPoint point(std::clamp(anchor.x(), available.left(), available.right() - width + 1),
                           std::clamp(anchor.y(), available.top(), available.bottom() - size.height() + 1));
        setGeometry(QRect(point, size));
        create();
        show();
        usable_ = handle() && isVisible() && available.contains(geometry()) && geometry().size() == size;
        requestUpdate();
    }
    QStringList visibleValues() const { return usable_ ? values_.first(count_) : QStringList(); }
    bool usable() const { return usable_ && isVisible() && screen() && screen()->availableGeometry().contains(geometry()); }
    int selected() const { return selected_; }
    void moveSelection(int delta) {
        if (!count_) return;
        selected_ = (selected_ + delta + count_) % count_;
        requestUpdate();
    }
protected:
    bool event(QEvent *event) override {
        if (event->type() == QEvent::UpdateRequest || event->type() == QEvent::Expose) paint();
        if (event->type() == QEvent::MouseButtonRelease) {
            const auto *mouse = static_cast<QMouseEvent *>(event);
            const int index = int(mouse->position().y()) / row_;
            if (client_ && mouse->button() == Qt::LeftButton && usable() &&
                QRect(QPoint(), size()).contains(mouse->position().toPoint()) && index >= 0 && index < count_ &&
                client_->chooser_.popup == this &&
                client_->accept(client_->chooser_, QStringLiteral("choice"), index)) return true;
            return QWindow::event(event);
        }
        return QWindow::event(event);
    }
private:
    void paint() {
        if (!isExposed() || !usable()) return;
        store_.resize(size());
        const QRegion area(QRect(QPoint(), size()));
        store_.beginPaint(area);
        QPainter painter(store_.paintDevice());
        painter.fillRect(QRect(QPoint(), size()), QGuiApplication::palette().base());
        painter.setPen(QGuiApplication::palette().text().color());
        painter.setFont(font_);
        for (int i = 0; i < count_; ++i) {
            if (i == selected_) painter.fillRect(QRect(0, i * row_, width(), row_), QGuiApplication::palette().highlight());
            painter.setPen(i == selected_ ? QGuiApplication::palette().highlightedText().color() : QGuiApplication::palette().text().color());
            const auto &bounds = bounds_[i];
            layouts_[i]->draw(&painter, QPointF(12 - bounds.left(),
                i * row_ + (row_ - bounds.height()) / 2 - bounds.top()));
        }
        painter.end();
        store_.endPaint();
        store_.flush(area);
    }
    QPointer<QtClient> client_;
    QBackingStore store_;
    QStringList values_;
    QFont font_;
    std::vector<std::unique_ptr<QTextLayout>> layouts_;
    std::vector<QRectF> bounds_;
    int row_ = 1, count_ = 0, selected_ = 0;
    bool usable_ = false;
};

QtClient::QtClient(QtClientHooks hooks, QObject *parent)
    : QObject(parent), hooks_(std::move(hooks)), bus_(QDBusConnection::sessionBus()),
      busName_(qEnvironmentVariable("BROTYPIST_NATIVE_BUS")), expiry_(new QTimer(this)), policyExpiry_(new QTimer(this)), pendingTimer_(new QTimer(this)) {
    expiry_->setSingleShot(true);
    connect(expiry_, &QTimer::timeout, this, [this] {
        const auto now = nowNs();
        if (ghost_.capability && (!now || now >= ghost_.lease)) revoke(ghost_);
        if (chooser_.capability && (!now || now >= chooser_.lease)) revoke(chooser_);
        scheduleExpiry(); scheduleChanged();
    });
    policyExpiry_->setSingleShot(true);
    connect(policyExpiry_, &QTimer::timeout, this, [this] { adoptPolicy(policy_); });
    pendingTimer_->setInterval(25);
    pendingTimer_->setTimerType(Qt::PreciseTimer);
    connect(pendingTimer_, &QTimer::timeout, this, &QtClient::resolvePending);
    if (!qGuiApp || busName_.isEmpty() || !bus_.isConnected() || thread() != qGuiApp->thread()) return;
    path_ = QStringLiteral("/org/brotypist/Client1/p%1/r%2").arg(QCoreApplication::applicationPid()).arg(QRandomGenerator::global()->generate64());
    if (!bus_.registerObject(path_, this, QDBusConnection::ExportAllSlots)) return;
    bus_.connect(QString(), QStringLiteral("/org/freedesktop/DBus/Local"),
        QStringLiteral("org.freedesktop.DBus.Local"), QStringLiteral("Disconnected"), this, SLOT(transportLost()));
    auto *watcher = new QDBusServiceWatcher(busName_, bus_, QDBusServiceWatcher::WatchForOwnerChange, this);
    connect(watcher, &QDBusServiceWatcher::serviceOwnerChanged, this,
        [this](const QString &, const QString &, const QString &owner) {
            if (pending_.active) finishPending(QStringLiteral("indeterminate"), {});
            revoke(); last_ = {}; policy_.clear(); policyExpiry_->stop(); clientId_ = 0; ++registration_; owner_ = owner;
            connected_ = !owner_.isEmpty();
            if (connected_) registerClient();
        });
    owner_ = bus_.interface()->serviceOwner(busName_).value();
    connected_ = !owner_.isEmpty();
    qGuiApp->installEventFilter(this);
    connect(qGuiApp, &QGuiApplication::focusObjectChanged, this, [this] { target(); invalidate(); });
    connect(qGuiApp, &QGuiApplication::focusWindowChanged, this, [this] { target(); invalidate(); });
    if (connected_) registerClient();
}
QtClient::~QtClient() {
    if (pending_.active) finishPending(QStringLiteral("indeterminate"), {});
    revoke();
    if (!path_.isEmpty()) bus_.unregisterObject(path_);
    if (qGuiApp) qGuiApp->removeEventFilter(this);
}
bool QtClient::authorized() const {
    return thread() == QThread::currentThread() && calledFromDBus() && connected_ && message().service() == owner_;
}
bool QtClient::policyLive() const {
    const auto now = nowNs();
    return connected_ && clientId_ && now &&
        policy_.value(QStringLiteral("policy_epoch")).toULongLong() && now < policy_.value(QStringLiteral("expires_ns")).toULongLong();
}
bool QtClient::policyValid() const {
    return policyLive() && policy_.value(QStringLiteral("enabled")).toBool();
}
void QtClient::registerClient() {
    const auto generation = ++registration_;
    auto call = QDBusMessage::createMethodCall(owner_, QStringLiteral("/org/brotypist/Native1"), QStringLiteral("org.brotypist.Native1"), QStringLiteral("Register"));
    // Metadata is safe without policy; it never queries committed text.
    const auto t = target();
    QVariantMap metadata{{"protocol", uint(1)}, {"backend", QStringLiteral("qt6-fcitx-client/1")},
        {"pid", uint(QCoreApplication::applicationPid())}, {"app_id", QCoreApplication::applicationFilePath()},
        {"focus", t.focused}, {"protected", t.protectedText}, {"editable", t.editable}, {"purpose", t.purpose}, {"composing", t.composition}, {"domain", QString()},
        {"field_token", t.field}, {"window_token", t.windowToken}, {"document_token", t.document},
        {"focus_generation", u64(focusGeneration_)}, {"pending_edit", pending_.active}};
    call.setArguments({QVariant::fromValue(QDBusObjectPath(path_)), metadata});
    auto *pending = new QDBusPendingCallWatcher(bus_.asyncCall(call, 2000), this);
    connect(pending, &QDBusPendingCallWatcher::finished, this, [this, generation](QDBusPendingCallWatcher *watcher) {
        QDBusPendingReply<qulonglong, QVariantMap> reply = *watcher;
        watcher->deleteLater();
        if (generation != registration_ || reply.isError()) return;
        clientId_ = reply.argumentAt<0>();
        adoptPolicy(reply.argumentAt<1>());
    });
}
QString QtClient::token(QObject *object) {
    if (!object) return {};
    const auto found = tokens_.constFind(object);
    if (found != tokens_.cend()) return found.value();
    const QString value = QString::number(++nextToken_);
    tokens_.insert(object, value);
    connect(object, &QObject::destroyed, this, [this, object, value] {
        tokens_.remove(object); userPreedit_.remove(object);
        for (auto it = scopeTokens_.begin(); it != scopeTokens_.end();) {
            if (it.key().startsWith(value + QLatin1Char(':'))) it = scopeTokens_.erase(it);
            else ++it;
        }
        invalidate();
    });
    return value;
}
QString QtClient::scopeToken(const QString &field, qlonglong origin) {
    const QString key = field + QLatin1Char(':') + QString::number(origin);
    if (scopeTokens_.size() >= 4096 && !scopeTokens_.contains(key)) return {};
    auto found = scopeTokens_.find(key);
    if (found == scopeTokens_.end()) found = scopeTokens_.insert(key, QString::number(++nextToken_));
    return found.value();
}
void QtClient::withdrawPolicy() {
    if (pending_.active) finishPending(QStringLiteral("indeterminate"), {});
    revoke(); policy_.clear(); policyExpiry_->stop(); last_ = {}; ++revision_;
    scheduleChanged();
}
void QtClient::observeScope(const Target &t) {
    const bool transition = scopeKnown_ && (scopeTarget_.field != t.field ||
        scopeTarget_.windowToken != t.windowToken || scopeTarget_.document != t.document ||
        scopeTarget_.focused != t.focused);
    scopeTarget_ = t; scopeKnown_ = true;
    if (transition) { ++focusGeneration_; withdrawPolicy(); }
}
QtClient::Target QtClient::target() {
    if (metadataQuery_) return {};
    metadataQuery_ = true;
    const auto metadataGuard = qScopeGuard([this] { metadataQuery_ = false; });
    Target t;
    const auto scopeGuard = qScopeGuard([this, &t] { observeScope(t); });
    t.object = hooks_.focusObject(); t.window = hooks_.focusWindow();
    t.field = token(t.object); t.windowToken = token(t.window); t.document = t.field;
    t.focused = t.object && t.window && t.window == QGuiApplication::focusWindow() && t.window->isActive();
    if (auto *widget = qobject_cast<QWidget *>(t.object.data()))
        t.focused = t.focused && widget->hasFocus() && widget->isVisible();
    t.composition = hooks_.composition();
    if (userPreedit_.value(t.object, false)) t.composition = QStringLiteral("active");
    if (!t.focused) return t;
    QInputMethodQueryEvent query(Qt::ImEnabled | Qt::ImHints);
    QCoreApplication::sendEvent(t.object, &query);
    if (!t.object || !t.window || t.object != hooks_.focusObject() || t.window != hooks_.focusWindow()) {
        t.focused = false; t.editable = false; return t;
    }
    if (!query.value(Qt::ImEnabled).isValid() || !query.value(Qt::ImHints).isValid()) return t;
    const auto hints = Qt::InputMethodHints(query.value(Qt::ImHints).toUInt());
    t.protectedText = bool(hints & (Qt::ImhHiddenText | Qt::ImhSensitiveData | Qt::ImhNoPredictiveText));
    t.purpose = t.protectedText ? QStringLiteral("sensitive") : QStringLiteral("text");
    if (auto *line = qobject_cast<QLineEdit *>(t.object.data())) {
        t.protectedText = t.protectedText || line->echoMode() != QLineEdit::Normal;
        t.editable = !line->isReadOnly();
    } else if (auto *edit = qobject_cast<QTextEdit *>(t.object.data())) {
        t.editable = !edit->isReadOnly(); t.document = token(edit->document());
    } else if (auto *plain = qobject_cast<QPlainTextEdit *>(t.object.data())) {
        t.editable = !plain->isReadOnly(); t.document = token(plain->document());
    } else if (t.object->inherits("QQuickTextInput")) {
        t.protectedText = t.protectedText || !t.object->property("echoMode").isValid() || t.object->property("echoMode").toInt() != 0;
        t.editable = t.object->property("readOnly").isValid() && !t.object->property("readOnly").toBool() && !t.object->property("inputMask").toString().size();
    } else if (t.object->inherits("QQuickTextEdit")) {
        t.editable = t.object->property("readOnly").isValid() && !t.object->property("readOnly").toBool();
    } else {
        t.editable = true;
    }
    t.editable = t.editable && query.value(Qt::ImEnabled).toBool() && !t.protectedText;
    if (t.protectedText) t.purpose = QStringLiteral("sensitive");
    if (t.object && !qobject_cast<QLineEdit *>(t.object.data()) &&
        !qobject_cast<QTextEdit *>(t.object.data()) && !qobject_cast<QPlainTextEdit *>(t.object.data()) &&
        !t.object->inherits("QQuickTextInput")) {
        QInputMethodQueryEvent position(Qt::ImAbsolutePosition | Qt::ImCursorPosition);
        QCoreApplication::sendEvent(t.object, &position);
        if (!t.object || !t.window || t.object != hooks_.focusObject() || t.window != hooks_.focusWindow()) {
            t.focused = false; t.editable = false; return t;
        }
        bool absoluteOk = false, caretOk = false;
        const qlonglong absolute = position.value(Qt::ImAbsolutePosition).toLongLong(&absoluteOk);
        const qlonglong caret = position.value(Qt::ImCursorPosition).toLongLong(&caretOk);
        t.document = absoluteOk && caretOk && caret >= 0 && absolute >= caret ? scopeToken(t.field, absolute - caret) : QString();
    }
    return t;
}
QtClient::State QtClient::capture(bool pendingReadback) {
    if (querying_ || (pending_.active && !pendingReadback)) return {};
    querying_ = true;
    const auto queryGuard = qScopeGuard([this] { querying_ = false; });
    State state; state.target = target();
    auto &t = state.target;
    if (!policyValid() || !t.focused || t.protectedText || !t.editable || t.composition != QStringLiteral("idle")) { last_ = {}; return state; }
    QInputMethodQueryEvent query(Qt::ImSurroundingText | Qt::ImCursorPosition | Qt::ImAnchorPosition | Qt::ImCurrentSelection | Qt::ImAbsolutePosition);
    QCoreApplication::sendEvent(t.object, &query);
    if (!t.object || t.object != hooks_.focusObject() || !query.value(Qt::ImSurroundingText).isValid() ||
        !query.value(Qt::ImCursorPosition).isValid() || !query.value(Qt::ImAnchorPosition).isValid()) return state;
    const auto surrounding = query.value(Qt::ImSurroundingText).toString();
    const int localCaret = query.value(Qt::ImCursorPosition).toInt(), localAnchor = query.value(Qt::ImAnchorPosition).toInt();
    if (auto *line = qobject_cast<QLineEdit *>(t.object.data())) {
        state.text = line->text(); state.caret = line->cursorPosition();
        state.anchor = line->hasSelectedText() ? (state.caret == line->selectionStart() ? line->selectionEnd() : line->selectionStart()) : state.caret;
        if (surrounding != state.text || localCaret != state.caret || localAnchor != state.anchor) return state;
        if (!line->inputMask().isEmpty()) return state;
    } else if (qobject_cast<QTextEdit *>(t.object.data()) || qobject_cast<QPlainTextEdit *>(t.object.data())) {
        QTextCursor cursor;
        if (auto *edit = qobject_cast<QTextEdit *>(t.object.data())) {
            if (edit->document()->characterCount() > 32001) return state;
            state.text = edit->toPlainText(); cursor = edit->textCursor();
        } else {
            auto *plain = qobject_cast<QPlainTextEdit *>(t.object.data());
            if (plain->document()->characterCount() > 32001) return state;
            state.text = plain->toPlainText(); cursor = plain->textCursor();
        }
        state.caret = cursor.position(); state.anchor = cursor.anchor();
        if (surrounding != cursor.block().text() || localCaret != state.caret - cursor.block().position()) return state;
    } else if (t.object->inherits("QQuickTextInput")) {
        state.text = t.object->property("text").toString(); state.caret = t.object->property("cursorPosition").toInt();
        const int start = t.object->property("selectionStart").toInt(), end = t.object->property("selectionEnd").toInt();
        state.anchor = state.caret == start ? end : start;
        if (surrounding != state.text || localCaret != state.caret || localAnchor != state.anchor) return state;
    } else {
        if (!query.value(Qt::ImAbsolutePosition).isValid() || !query.value(Qt::ImCurrentSelection).isValid()) return state;
        bool absoluteOk = false;
        const qlonglong absolute = query.value(Qt::ImAbsolutePosition).toLongLong(&absoluteOk);
        if (!absoluteOk || absolute < localCaret || localCaret < 0 || localAnchor < 0 ||
            localCaret > surrounding.size() || localAnchor > surrounding.size()) return state;
        const QString document = scopeToken(t.field, absolute - localCaret);
        if (t.document.isEmpty() || t.document != document) return state;
        state.text = surrounding; state.caret = localCaret; state.anchor = localAnchor;
        const QString selected = state.text.mid(std::min(localCaret, localAnchor), std::abs(localCaret - localAnchor));
        if (query.value(Qt::ImCurrentSelection).toString() != selected) return state;
    }
    const auto fresh = target();
    if (!t.object || !t.window || t.object != fresh.object || t.window != fresh.window) return state;
    if (!policyValid() || fresh.document != t.document ||
        !fresh.focused || fresh.protectedText || !fresh.editable || fresh.composition != QStringLiteral("idle") ||
        !bounded(state.text) || !boundary(state.text, state.caret) || !boundary(state.text, state.anchor)) return state;
    if (last_.valid && (last_.target.field != t.field || last_.target.windowToken != t.windowToken || last_.target.document != t.document || last_.text != state.text || last_.caret != state.caret || last_.anchor != state.anchor)) ++revision_;
    state.valid = true;
    state.snapshot = {{"field_token", t.field}, {"window_token", t.windowToken}, {"document_token", t.document},
        {"revision", u64(revision_)}, {"user_revision", u64(userRevision_)}, {"notification_revision", u64(notificationRevision_)},
        {"offset_unit", QStringLiteral("utf16")}, {"text", state.text}, {"caret", uint(state.caret)}, {"anchor", uint(state.anchor)},
        {"focus", true}, {"protected", false}, {"editable", true}, {"purpose", t.purpose}, {"composing", t.composition}, {"domain", QString()}};
    if (!pendingReadback) { last_ = state; observedTarget_ = fresh; }
    return state;
}
QVariantMap QtClient::Metadata() {
    if (!authorized()) return {};
    const auto t = target();
    return {{"protocol", uint(1)}, {"backend", QStringLiteral("qt6-fcitx-client/1")}, {"pid", uint(QCoreApplication::applicationPid())},
        {"app_id", QCoreApplication::applicationFilePath()}, {"focus", t.focused}, {"protected", t.protectedText},
        {"editable", t.editable}, {"purpose", t.purpose}, {"composing", t.composition}, {"domain", QString()},
        {"field_token", t.field}, {"window_token", t.windowToken}, {"document_token", t.document},
        {"focus_generation", u64(focusGeneration_)}, {"pending_edit", pending_.active}};
}
QVariantMap QtClient::Capture(qulonglong policy_epoch) {
    if (!authorized() || policy_epoch != policy_.value("policy_epoch").toULongLong()) return {};
    if (pending_.active) { resolvePending(); return {}; }
    return capture().snapshot;
}
bool QtClient::matches(const State &state, const QVariantMap &offer) const {
    if (!state.valid || !policyValid() || offer.value("policy_epoch").toULongLong() != policy_.value("policy_epoch").toULongLong() ||
        nowNs() >= offer.value("expires_ns").toULongLong() || offer.value("offset_unit").toString() != QStringLiteral("utf16") ||
        offer.value("text_sha256").toString() != hash(state.text)) return false;
    for (const char *key : {"field_token", "window_token", "document_token", "revision", "user_revision", "caret", "anchor"})
        if (!offer.contains(QLatin1String(key)) || offer.value(QLatin1String(key)) != state.snapshot.value(QLatin1String(key))) return false;
    return true;
}
bool QtClient::feature(const QString &mode) const {
    const QString flag = mode == QStringLiteral("completion") ? QStringLiteral("predictions") : mode;
    return (flag == QStringLiteral("predictions") || flag == QStringLiteral("correction") || flag == QStringLiteral("emoji") || flag == QStringLiteral("synonyms") || flag == QStringLiteral("alternatives")) && policy_.value(flag).toBool();
}
bool QtClient::ghostGeometry(const State &s, const QString &text, QRectF *out) const {
    QPointer<QObject> object = s.target.object;
    QPointer<QWindow> window = s.target.window;
    if (!s.valid || !object || !window || object != hooks_.focusObject() || window != hooks_.focusWindow()) return false;
    const QRectF cursor = qGuiApp->inputMethod()->cursorRectangle();
    if (!object || !window || !cursor.isValid()) return false;
    QRectF viewport;
    QPointer<QWidget> painterWidget;
    QFont font;
    QString context;
    int start = 0;
    qreal baseline = 0;
    QTextLayout shaped;
    QTextLayout *layout = nullptr;
    auto mapRect = [&window](QWidget *widget, const QRectF &rect) {
        return rect.translated(window->mapFromGlobal(widget->mapToGlobal(QPoint())));
    };
    if (auto *lineEdit = qobject_cast<QLineEdit *>(object.data())) {
        // Qt 6.12 QLineEdit::paintEvent clips to SE_LineEditContents minus
        // effectiveTextMargins. With no side widgets, these are textMargins().
        // Side-widget private margins are not guessed through an installed ABI.
        if (!lineEdit->actions().isEmpty() || lineEdit->isClearButtonEnabled() ||
            !(lineEdit->alignment() & Qt::AlignVCenter) ||
            (lineEdit->alignment() & (Qt::AlignLeft | Qt::AlignRight | Qt::AlignHCenter | Qt::AlignJustify)) != Qt::AlignLeft) return false;
        for (auto *child : lineEdit->findChildren<QWidget *>(QString(), Qt::FindDirectChildrenOnly))
            if (child->isVisible()) return false;
        QStyleOptionFrame panel;
        panel.initFrom(lineEdit); panel.rect = lineEdit->contentsRect();
        panel.state |= QStyle::State_Sunken;
        panel.lineWidth = lineEdit->hasFrame() ? lineEdit->style()->pixelMetric(QStyle::PM_DefaultFrameWidth, &panel, lineEdit) : 0;
        if (!object || !window || object != hooks_.focusObject()) return false;
        const QRect contents = lineEdit->style()->subElementRect(QStyle::SE_LineEditContents, &panel, lineEdit);
        if (!object || !window || object != hooks_.focusObject()) return false;
        const QRect paint = contents.marginsRemoved(lineEdit->textMargins());
        viewport = mapRect(lineEdit, paint); painterWidget = lineEdit;
        font = lineEdit->font(); context = s.text; start = s.caret;
        const QFontMetrics metrics(font);
        const int height = std::max(metrics.height(), metrics.boundingRect(context + text).height());
        const qreal top = paint.y() + (paint.height() - height + 1) / 2;
        baseline = mapRect(lineEdit, QRectF(0, top + metrics.ascent(), 0, 0)).top();
    } else if (qobject_cast<QTextEdit *>(object.data()) || qobject_cast<QPlainTextEdit *>(object.data())) {
        QTextCursor nativeCursor;
        if (auto *edit = qobject_cast<QTextEdit *>(object.data())) {
            nativeCursor = edit->textCursor(); painterWidget = edit->viewport();
        } else {
            auto *plain = qobject_cast<QPlainTextEdit *>(object.data());
            nativeCursor = plain->textCursor(); painterWidget = plain->viewport();
        }
        if (!painterWidget) return false;
        viewport = mapRect(painterWidget, painterWidget->contentsRect());
        layout = nativeCursor.block().layout();
        start = nativeCursor.position() - nativeCursor.block().position();
        if (!layout || layout->preeditAreaPosition() != start || layout->preeditAreaText() != text) return false;
        font = layout->font();
    } else if (object->inherits("QQuickTextInput")) {
        auto property = [&object](const char *name) { return object ? object->property(name) : QVariant(); };
        const auto transform = qGuiApp->inputMethod()->inputItemTransform();
        if (!qFuzzyCompare(transform.m11(), qreal(1)) || !qFuzzyCompare(transform.m22(), qreal(1)) ||
            !qFuzzyIsNull(transform.m12()) || !qFuzzyIsNull(transform.m21()) ||
            !qFuzzyIsNull(transform.m13()) || !qFuzzyIsNull(transform.m23()) || !qFuzzyCompare(transform.m33(), qreal(1)) ||
            property("wrapMode").toInt() != 0 || property("horizontalAlignment").toInt() != int(Qt::AlignLeft)) return false;
        const QVariant width = property("width"), height = property("height"), base = property("baselineOffset");
        const QVariant left = property("leftPadding"), right = property("rightPadding"), top = property("topPadding"), bottom = property("bottomPadding");
        if (!object || !width.isValid() || !height.isValid() || !base.isValid() ||
            !left.isValid() || !right.isValid() || !top.isValid() || !bottom.isValid()) return false;
        const QRectF paint(left.toReal(), top.toReal(), width.toReal() - left.toReal() - right.toReal(),
                           height.toReal() - top.toReal() - bottom.toReal());
        if (!property("contentHeight").isValid() || property("contentHeight").toReal() > paint.height()) return false;
        viewport = transform.mapRect(paint).intersected(qGuiApp->inputMethod()->inputItemClipRectangle());
        baseline = transform.map(QPointF(0, base.toReal())).y();
        const QVariant nativeFont = property("font");
        if (!nativeFont.isValid()) return false;
        font = nativeFont.value<QFont>(); context = s.text; start = s.caret;
    } else {
        // Generic/Quick TextEdit query consumers remain eligible for native
        // choosers and edits; a widget bounding box is not text-layout evidence.
        return false;
    }
    if (!object || !window || !viewport.isValid()) return false;
    viewport = viewport.intersected(QRectF(QPointF(), QSizeF(window->size())));
    if (!layout) {
        for (const auto character : context + text) {
            const auto direction = character.direction();
            if (direction == QChar::DirR || direction == QChar::DirAL ||
                direction == QChar::DirRLE || direction == QChar::DirRLO || direction == QChar::DirRLI) return false;
        }
        shaped.setText(context); shaped.setFont(font); shaped.setPreeditArea(start, text);
        QTextOption option; option.setWrapMode(QTextOption::NoWrap); option.setTextDirection(Qt::LeftToRight);
        shaped.setTextOption(option);
        shaped.beginLayout();
        auto line = shaped.createLine();
        if (line.isValid()) line.setLineWidth(16000000);
        shaped.endLayout();
        layout = &shaped;
    }
    const auto line = layout->lineForTextPosition(start);
    const int end = start + int(text.size());
    if (!line.isValid() || start < line.textStart() || end > line.textStart() + line.textLength()) return false;
    const qreal x = line.cursorToX(start), advance = line.cursorToX(end) - x;
    if (advance < 0) return false;
    const qreal offsetY = layout == &shaped ? baseline - line.ascent() : cursor.top() - line.y();
    QRectF ink;
    const QFontMetricsF metrics(font);
    qreal underline = metrics.underlinePos(), thickness = std::max(qreal(1), metrics.lineWidth());
    for (const auto &run : line.glyphRuns(start, int(text.size()))) {
        ink = ink.united(run.boundingRect());
        const auto raw = run.rawFont();
        if (raw.isValid()) {
            underline = std::max(underline, raw.underlinePosition());
            thickness = std::max(thickness, raw.lineThickness());
        }
    }
    const QRectF leftInk = ink.translated(cursor.left() - x, offsetY);
    const QRectF rightInk = ink.translated(cursor.right() - x, offsetY);
    // Include advance/whitespace and native underline ink, not only glyph
    // advance widths. The cursor-width envelope is conservative about rounding.
    QRectF occupied(cursor.left(), line.y() + offsetY, cursor.width() + advance, line.height());
    occupied = occupied.united(leftInk).united(rightInk);
    occupied = occupied.united(QRectF(cursor.left(), line.y() + offsetY + line.ascent() + underline,
        cursor.width() + advance, thickness));
    if (!viewport.contains(occupied)) return false;
    if (painterWidget) {
        const QPoint origin = window->mapFromGlobal(painterWidget->mapToGlobal(QPoint()));
        if (!painterWidget->visibleRegion().contains(occupied.translated(-origin).toAlignedRect())) return false;
    }
    if (!object || !window || object != hooks_.focusObject() || window != hooks_.focusWindow()) return false;
    if (out) *out = viewport;
    return true;
}
QVariantMap QtClient::Present(const QVariantMap &offer) {
    if (!authorized()) return refusal("unauthorized");
    State state = capture();
    if (!matches(state, offer)) return refusal("stale-or-unsupported-target");
    const QString mode = offer.value("mode").toString();
    if (!feature(mode) || !offer.value("offer_id").toULongLong()) return refusal("disabled-feature");
    const bool inlineMode = mode == QStringLiteral("completion") || mode == QStringLiteral("correction");
    if (inlineMode && mode == QStringLiteral("completion") &&
        ghost_.offer.value("mode").toString() == QStringLiteral("correction") && live(ghost_, state))
        return refusal("higher-priority-native-preedit");
    const auto start64 = offer.value("start").toULongLong(), length64 = offer.value("length").toULongLong();
    if (!offer.contains("start") || !offer.contains("length") || start64 > qulonglong(state.text.size()) || length64 > qulonglong(state.text.size()) - start64 ||
        !boundary(state.text, int(start64)) || !boundary(state.text, int(start64 + length64))) return refusal("invalid-range");
    if (state.caret != state.anchor && (start64 != qulonglong(std::min(state.caret, state.anchor)) || length64 != qulonglong(std::abs(state.caret - state.anchor)))) return refusal("selection-range-mismatch");
    QInputMethodQueryEvent query(Qt::ImCursorRectangle | Qt::ImFont);
    QCoreApplication::sendEvent(state.target.object, &query);
    const QRectF cursor = qGuiApp->inputMethod()->cursorRectangle();
    const QFont font = query.value(Qt::ImFont).value<QFont>();
    if (!state.target.object || !state.target.window || !query.value(Qt::ImCursorRectangle).isValid() ||
        !query.value(Qt::ImFont).isValid() || !cursor.isValid() || !state.target.window->screen()) return refusal("unavailable-native-geometry");
    Presentation &p = inlineMode ? ghost_ : chooser_;
    revoke(p); // A chooser never destroys an existing native ghost receipt.
    if (inlineMode) {
        const QString text = offer.value("text").toString();
        if (qGuiApp->inputMethod()->inputDirection() != Qt::LeftToRight || state.caret != state.anchor ||
            text.isEmpty() || !bounded(text) || text.contains(QChar::LineFeed) || text.contains(QChar::CarriageReturn) ||
            text.contains(QChar::Tabulation) || text.contains(QChar::LineSeparator) || text.contains(QChar::ParagraphSeparator))
            return refusal("preedit-not-fully-visible");
        QTextCharFormat format; format.setForeground(QGuiApplication::palette().color(QPalette::Disabled, QPalette::Text));
        format.setUnderlineStyle(QTextCharFormat::DashUnderline);
        QList<QInputMethodEvent::Attribute> attributes{
            {QInputMethodEvent::TextFormat, 0, int(text.size()), format},
            {QInputMethodEvent::Cursor, 0, 0, {}}};
        p.ghostTarget = state.target.object;
        dispatching_ = true;
        QInputMethodEvent event(text, attributes);
        ownEvent_ = &event;
        const bool delivered = QCoreApplication::sendEvent(p.ghostTarget, &event);
        ownEvent_ = nullptr;
        dispatching_ = false;
        if (!p.ghostTarget || !delivered || !event.isAccepted()) { revoke(p); return refusal("native-preedit-refused"); }
        p.shownText = text;
        p.cursor = qGuiApp->inputMethod()->cursorRectangle();
        if (!ghostGeometry(state, text, &p.clip)) { revoke(p); return refusal("preedit-not-fully-visible"); }
    } else {
        const QStringList values = stringList(offer.value("values"));
        if (values.isEmpty() || values.size() > 256) return refusal("invalid-choices");
        for (const auto &value : values) if (!bounded(value)) return refusal("invalid-choices");
        auto *window = new ChoiceWindow(this, state.target.window, values, font,
            state.target.window->mapToGlobal(cursor.bottomLeft().toPoint()));
        p.popup = window;
        p.shownValues = window->visibleValues();
        if (p.shownValues.isEmpty()) { revoke(p); return refusal("choices-not-fully-visible"); }
    }
    const auto after = capture();
    if (!matches(after, offer)) { revoke(p); return refusal("target-changed-during-presentation"); }
    p.offer = offer; p.witness = after; p.capability = ++nextCapability_;
    const auto now = nowNs();
    p.lease = std::min({offer.value("expires_ns").toULongLong(), policy_.value("expires_ns").toULongLong(), now + 2000000000ULL});
    if (!now || p.lease <= now) { revoke(p); return refusal("expired"); }
    scheduleExpiry();
    return {{"presented", true}, {"capability_id", u64(p.capability)}, {"offer_id", offer.value("offer_id")},
        {"expires_ns", u64(p.lease)}, {"shown_text", p.shownText}, {"shown_values", p.shownValues}};
}
QtClient::Presentation *QtClient::presentation(qulonglong capability) {
    if (capability && capability == ghost_.capability) return &ghost_;
    if (capability && capability == chooser_.capability) return &chooser_;
    return nullptr;
}
bool QtClient::live(const Presentation &p, const State &state) const {
    if (!p.capability || nowNs() >= p.lease || !matches(state, p.offer) ||
        state.target.object != p.witness.target.object || state.target.window != p.witness.target.window) return false;
    if (!p.shownText.isEmpty()) {
        QRectF viewport;
        if (!p.ghostTarget || qGuiApp->inputMethod()->cursorRectangle() != p.cursor ||
            !ghostGeometry(state, p.shownText, &viewport) || viewport != p.clip) return false;
    }
    return p.shownValues.isEmpty() || (p.popup && p.popup->usable());
}
bool QtClient::Arm(const QVariantMap &acceptance) {
    if (!authorized()) return false;
    auto *p = presentation(acceptance.value("capability_id").toULongLong());
    if (!p || !policyValid() || nowNs() >= p->lease ||
        acceptance.value("offer_id") != p->offer.value("offer_id") ||
        acceptance.value("policy_epoch").toULongLong() != policy_.value("policy_epoch").toULongLong() ||
        acceptance.value("expires_ns").toULongLong() != p->lease ||
        !acceptance.contains("shown_text") || acceptance.value("shown_text").toString() != p->shownText ||
        !acceptance.contains("shown_values") || stringList(acceptance.value("shown_values")) != p->shownValues ||
        !acceptance.contains("word_text") || !acceptance.contains("all_text")) return false;
    if (!live(*p, capture())) { revoke(*p); return false; }
    const QString word = acceptance.value("word_text").toString();
    const QString all = acceptance.value("all_text").toString();
    if (!bounded(word) || !bounded(all) || (!p->shownText.isEmpty() && all.isEmpty())) return false;
    // Exact receipt binding authorizes the core's Suggestion::accept outputs,
    // not a second client-side word/trailing-space implementation.
    if (p->armed) return word == p->word && all == p->all;
    p->word = word; p->all = all; p->armed = true;
    return true;
}
void QtClient::scheduleExpiry() {
    expiry_->stop();
    qulonglong deadline = 0;
    for (const auto *p : {&ghost_, &chooser_})
        if (p->capability && (!deadline || p->lease < deadline)) deadline = p->lease;
    const auto now = nowNs();
    if (deadline) expiry_->start(!now || deadline <= now ? 0 : int((deadline - now + 999999) / 1000000));
}
void QtClient::revoke(Presentation &p) {
    const QPointer<QObject> object = p.ghostTarget;
    const QPointer<ChoiceWindow> popup = p.popup;
    p = {};
    if (popup) { popup->hide(); popup->deleteLater(); }
    if (object) {
        const bool previous = dispatching_; dispatching_ = true;
        QInputMethodEvent empty;
        const QEvent *previousEvent = ownEvent_; ownEvent_ = &empty;
        QCoreApplication::sendEvent(object, &empty);
        ownEvent_ = previousEvent; dispatching_ = previous;
    }
    scheduleExpiry();
}
void QtClient::revoke() {
    revoke(ghost_); revoke(chooser_);
}
void QtClient::Clear(qulonglong capability_id) {
    if (!authorized()) return;
    if (!capability_id) { revoke(); last_ = {}; }
    else if (auto *p = presentation(capability_id)) revoke(*p);
}
void QtClient::adoptPolicy(const QVariantMap &policy) {
    QVariantMap next = policy;
    const auto expiryRequested = next.value("expires_ns").toULongLong();
    if (!expiryRequested) { withdrawPolicy(); return; }
    const auto scope = target();
    if (next.value("focus_generation").toULongLong() != focusGeneration_ ||
        next.value("field_token").toString() != scope.field ||
        next.value("window_token").toString() != scope.windowToken ||
        next.value("document_token").toString() != scope.document) return;
    const QVariant shortcuts = next.value(QStringLiteral("shortcuts"));
    if (shortcuts.metaType() == QMetaType::fromType<QDBusArgument>()) {
        QVariantMap normalized;
        const auto entries = qdbus_cast<QMap<QString, QString>>(shortcuts);
        for (auto it = entries.cbegin(); it != entries.cend(); ++it) normalized.insert(it.key(), it.value());
        next.insert(QStringLiteral("shortcuts"), normalized);
    }
    QVariantMap oldSettings = policy_, nextSettings = next;
    oldSettings.remove(QStringLiteral("expires_ns")); nextSettings.remove(QStringLiteral("expires_ns"));
    const auto now = nowNs(), expiry = next.value("expires_ns").toULongLong();
    const auto oldExpiry = policy_.value("expires_ns").toULongLong();
    const bool renewal = now && oldExpiry > now && expiry >= oldExpiry && oldSettings == nextSettings;
    if (!renewal) {
        if (pending_.active) finishPending(QStringLiteral("indeterminate"), {});
        revoke(); last_ = {}; ++revision_;
    }
    policyExpiry_->stop(); policy_ = next;
    if (!now || !policy_.value("policy_epoch").toULongLong() || expiry <= now) policy_.clear();
    else policyExpiry_->start(int(std::min<qulonglong>((expiry - now) / 1000000 + 1, std::numeric_limits<int>::max())));
    scheduleChanged();
}
void QtClient::transportLost() {
    if (pending_.active) finishPending(QStringLiteral("indeterminate"), {});
    revoke(); last_ = {}; policy_.clear(); policyExpiry_->stop(); connected_ = false; clientId_ = 0; ++registration_;
}
void QtClient::Configure(const QVariantMap &policy) { if (authorized()) adoptPolicy(policy); }
void QtClient::invalidate(bool ordinaryInput) {
    if (pending_.active && (ordinaryInput || !dispatching_)) finishPending(QStringLiteral("indeterminate"), {});
    revoke(); ++revision_; if (ordinaryInput) ++userRevision_; last_ = {}; scheduleChanged();
}
void QtClient::nativeChanged() {
    if (dispatching_ || editing_ || querying_ || metadataQuery_) return;
    if (pending_.active) { resolvePending(); if (pending_.active) return; }
    const auto metadata = target();
    if (metadata.field != observedTarget_.field || metadata.windowToken != observedTarget_.windowToken ||
        metadata.focused != observedTarget_.focused || metadata.protectedText != observedTarget_.protectedText ||
        metadata.editable != observedTarget_.editable || metadata.composition != observedTarget_.composition) {
        ++revision_;
        revoke();
    }
    observedTarget_ = metadata;
    const auto current = capture();
    if (ghost_.capability && !live(ghost_, current)) revoke(ghost_);
    if (chooser_.capability && !live(chooser_, current)) revoke(chooser_);
    scheduleChanged();
}
void QtClient::scheduleChanged() {
    if (scheduled_ || editing_) return;
    scheduled_ = true;
    QTimer::singleShot(0, this, [this] { scheduled_ = false; changed(); });
}
void QtClient::changed() {
    if (editing_ || !connected_ || !clientId_) return;
    if (pending_.active) { resolvePending(); if (pending_.active) return; }
    const auto state = capture();
    const auto t = state.valid ? state.target : target();
    ++notificationRevision_;
    event({{"kind", QStringLiteral("changed")}, {"field_token", t.field}, {"window_token", t.windowToken}, {"document_token", t.document},
        {"revision", u64(revision_)}, {"user_revision", u64(userRevision_)}, {"notification_revision", u64(notificationRevision_)},
        {"focus", t.focused}, {"protected", t.protectedText}, {"composing", t.composition}, {"focus_generation", u64(focusGeneration_)}});
}
void QtClient::event(QVariantMap value) {
    if (!clientId_ || !connected_) return;
    if (pendingEvents_ >= 32) { revoke(); last_ = {}; policy_.clear(); policyExpiry_->stop(); return; }
    auto call = QDBusMessage::createMethodCall(owner_, QStringLiteral("/org/brotypist/Native1"), QStringLiteral("org.brotypist.Native1"), QStringLiteral("Event"));
    call.setArguments({u64(clientId_), value});
    ++pendingEvents_;
    auto *pending = new QDBusPendingCallWatcher(bus_.asyncCall(call, 2000), this);
    connect(pending, &QDBusPendingCallWatcher::finished, this, [this, generation = registration_](QDBusPendingCallWatcher *watcher) {
        --pendingEvents_;
        if (generation == registration_ && watcher->isError()) withdrawPolicy();
        watcher->deleteLater();
    });
}
QString QtClient::shortcut(const QString &action) const {
    const QVariant value = policy_.value(QStringLiteral("shortcuts"));
    if (value.metaType() == QMetaType::fromType<QDBusArgument>()) return qdbus_cast<QMap<QString, QString>>(value).value(action);
    return mapValue(value).value(action).toString();
}
bool QtClient::sendAction(const QString &action) {
    if (!policyLive()) return false;
    QVariantMap value{{"kind", QStringLiteral("action")}, {"action", action}};
    for (const char *key : {"field_token", "window_token", "document_token", "focus_generation", "policy_epoch"})
        value.insert(QLatin1String(key), policy_.value(QLatin1String(key)));
    invalidate(true);
    if (!policyLive() || policy_.value("policy_epoch") != value.value("policy_epoch") ||
        policy_.value("focus_generation") != value.value("focus_generation")) return false;
    event(value);
    return true;
}
bool QtClient::key(const QKeyEvent &key) {
    if (key.type() != QEvent::KeyPress) return false;
    target(); // Detect actual scope transitions before even metadata-only keys.
    if (policyLive()) {
        for (const QString &action : {QStringLiteral("app_toggle"), QStringLiteral("toggle")}) {
            if (keyMatches(key, shortcut(action))) {
                // Metadata-only control remains available while disabled.
                // Do not Capture or query committed field text to re-enable.
                return sendAction(action);
            }
        }
    }
    const bool word = keyMatches(key, shortcut(QStringLiteral("word")));
    const bool all = keyMatches(key, shortcut(QStringLiteral("all")));
    if (!key.isAutoRepeat() && (word || all)) {
        const auto state = capture();
        if (ghost_.capability && !live(ghost_, state)) revoke(ghost_);
        if (chooser_.capability && !live(chooser_, state)) revoke(chooser_);
        if (ghost_.capability) {
            if (ghost_.armed && accept(ghost_, word ? QStringLiteral("word") : QStringLiteral("all"))) return true;
            // Unarmed/higher-priority ghost must not fall through into a choice.
        } else {
            const QString mode = chooser_.offer.value("mode").toString();
            if ((mode == QStringLiteral("emoji") || mode == QStringLiteral("synonyms")) &&
                chooser_.armed && accept(chooser_, QStringLiteral("choice"), 0)) return true;
        }
        invalidate(true);
        return false; // Alternatives never steal word/all.
    }
    if (chooser_.armed && key.modifiers() == Qt::NoModifier && live(chooser_, capture())) {
        if (key.key() == Qt::Key_Up || key.key() == Qt::Key_Down) {
            chooser_.popup->moveSelection(key.key() == Qt::Key_Up ? -1 : 1);
            return true;
        }
        if (!key.isAutoRepeat()) {
            if ((key.key() == Qt::Key_Return || key.key() == Qt::Key_Enter) &&
                accept(chooser_, QStringLiteral("choice"), chooser_.popup->selected())) return true;
            if (key.key() >= Qt::Key_1 && key.key() <= Qt::Key_9 &&
                accept(chooser_, QStringLiteral("choice"), key.key() - Qt::Key_1)) return true;
        }
    }
    if (policyValid()) {
        if (key.key() == Qt::Key_Escape && key.modifiers() == Qt::NoModifier && !key.isAutoRepeat()) {
            const State before = capture();
            bool dismissed = false;
            for (const auto *p : {&ghost_, &chooser_}) {
                if (!p->armed || !live(*p, before)) continue;
                event({{"kind", QStringLiteral("dismissed")}, {"offer_id", p->offer.value("offer_id")},
                    {"capability_id", u64(p->capability)}, {"before", before.snapshot},
                    {"accepted_ns", u64(nowNs())}, {"bypass", false}});
                dismissed = true;
            }
            if (dismissed) {
                const QString behavior = policy_.value(QStringLiteral("escape_behavior")).toString();
                invalidate(true);
                return behavior == QStringLiteral("pause") || behavior == QStringLiteral("app");
            }
        }
        for (const QString &action : {QStringLiteral("force"), QStringLiteral("alternatives"), QStringLiteral("synonyms")}) {
            if (keyMatches(key, shortcut(action))) {
                const auto metadata = target();
                if (!metadata.focused || metadata.protectedText || !metadata.editable ||
                    metadata.composition != QStringLiteral("idle")) break;
                return sendAction(action);
            }
        }
    }
    invalidate(true);
    return false;
}
void QtClient::finishPending(const QString &outcome, const State &after) {
    if (!pending_.active || pending_.finalSent || finalizingPending_) return;
    finalizingPending_ = true;
    pendingTimer_->stop();
    pending_.confirmation.insert(QStringLiteral("outcome"), outcome);
    if (after.valid) pending_.confirmation.insert(QStringLiteral("after"), after.snapshot);
    pending_.finalAfter = after;
    pending_.finalSent = true;
    const auto id = pending_.confirmation.value("acceptance_id").toULongLong();
    // Asynchronous Event ACK means the authenticated broker has validated and
    // enqueued the frozen proof. Sending alone must not release Capture.
    auto call = QDBusMessage::createMethodCall(pending_.owner, QStringLiteral("/org/brotypist/Native1"),
        QStringLiteral("org.brotypist.Native1"), QStringLiteral("Event"));
    call.setArguments({u64(pending_.clientId), pending_.confirmation});
    ++pendingEvents_;
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call, 1000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, id](QDBusPendingCallWatcher *reply) {
        --pendingEvents_;
        const bool current = pending_.active && pending_.finalSent &&
            pending_.confirmation.value("acceptance_id").toULongLong() == id;
        if (current && !reply->isError()) {
            const State actual = pending_.finalAfter;
            pending_ = {};
            last_ = actual.valid ? actual : State{};
            scheduleChanged();
        } else if (current) {
            // No redispatch/retry or authored-text fallback. Retain the one
            // unconfirmed receipt and quarantine even after transport failure.
            revoke(); policy_.clear(); policyExpiry_->stop(); last_ = {};
        }
        reply->deleteLater();
    });
    finalizingPending_ = false;
}
void QtClient::resolvePending() {
    if (!pending_.active || pending_.finalSent || resolvingPending_ || finalizingPending_ || dispatching_ ||
        editing_ || querying_ || metadataQuery_) return;
    resolvingPending_ = true;
    const auto guard = qScopeGuard([this] { resolvingPending_ = false; });
    const auto id = pending_.confirmation.value("acceptance_id").toULongLong();
    const auto now = nowNs();
    if (!now || now >= pending_.deadline || !policyValid() || pending_.registration != registration_ ||
        pending_.policyEpoch != policy_.value("policy_epoch").toULongLong() ||
        userRevision_ != pending_.before.snapshot.value("user_revision").toULongLong()) {
        finishPending(QStringLiteral("indeterminate"), {});
        return;
    }
    const State after = capture(true); // Real owner-thread native readback.
    if (!pending_.active || pending_.confirmation.value("acceptance_id").toULongLong() != id) return;
    if (!after.valid) return; // Unknown native state remains quarantined/bounded.
    if (after.target.object != pending_.before.target.object || after.target.window != pending_.before.target.window ||
        after.target.document != pending_.before.target.document) {
        finishPending(QStringLiteral("indeterminate"), {});
        return;
    }
    if (after.text == pending_.expected && after.caret == pending_.caret && after.anchor == pending_.caret) {
        finishPending(nowNs() < pending_.deadline ? QStringLiteral("applied") : QStringLiteral("indeterminate"), after);
    } else if (after.text != pending_.before.text || after.caret != pending_.before.caret ||
               after.anchor != pending_.before.anchor) {
        finishPending(QStringLiteral("indeterminate"), after);
    }
    // Identical before text/selection is not evidence of rejection. The single
    // original dispatch may still be queued inside a conforming native target.
}
bool QtClient::accept(Presentation &p, const QString &action, int index) {
    if (pending_.active || pendingEvents_ >= 31) return false; // Reserve final-confirmation capacity.
    if (!p.armed) return false;
    const State before = capture();
    if (!live(p, before)) { revoke(p); return false; }
    QString accepted;
    if (action == QStringLiteral("choice")) {
        if (!p.popup || index < 0 || index >= p.shownValues.size()) return false;
        accepted = p.shownValues[index]; // Exact displayed choice, no extra space.
    } else if (action == QStringLiteral("all")) accepted = p.all;
    else if (action == QStringLiteral("word")) accepted = p.word;
    else return false;
    if (accepted.isEmpty() || !bounded(accepted)) return false;
    const QVariantMap offer = p.offer;
    const qulonglong capability = p.capability, acceptance = ++nextAcceptance_;
    const qulonglong acceptanceLease = p.lease;
    const int start = int(offer.value("start").toUInt()), length = int(offer.value("length").toUInt());
    // Any committed edit invalidates all overlapping field witnesses, but merely
    // presenting/clearing/expiring one mode leaves the other receipt intact.
    revoke();
    const State ready = capture();
    if (!matches(ready, offer) || ready.target.object != before.target.object || !policyValid() || nowNs() >= acceptanceLease) return false;
    PendingEdit pending;
    pending.before = before;
    pending.expected = before.text.left(start) + accepted + before.text.mid(start + length);
    pending.caret = start + int(accepted.size());
    pending.deadline = acceptanceLease; pending.registration = registration_;
    pending.policyEpoch = policy_.value("policy_epoch").toULongLong();
    pending.owner = owner_; pending.clientId = clientId_;
    pending.confirmation = {{"kind", QStringLiteral("accepted")}, {"acceptance_id", u64(acceptance)}, {"offer_id", offer.value("offer_id")},
        {"capability_id", u64(capability)}, {"action", action}, {"start", uint(start)}, {"length", uint(length)},
        {"text", accepted}, {"before", before.snapshot}, {"expires_ns", u64(acceptanceLease)},
        {"policy_epoch", u64(pending.policyEpoch)}, {"accepted_ns", u64(nowNs())}};
    if (index >= 0) pending.confirmation.insert(QStringLiteral("choice_index"), uint(index));
    pending.active = true; pending_ = std::move(pending); // Install BEFORE dispatch.
    pendingTimer_->start();
    editing_ = true; dispatching_ = true;
    QInputMethodEvent edit;
    if (before.caret != before.anchor) edit.setCommitString(accepted, 0, 0);
    else edit.setCommitString(accepted, start - before.caret, length);
    ownEvent_ = &edit;
    const auto dispatchNs = nowNs();
    if (!dispatchNs || dispatchNs >= acceptanceLease || !policyValid() ||
        !before.target.object || !before.target.window ||
        before.target.object != hooks_.focusObject() || before.target.window != hooks_.focusWindow()) {
        // Nothing was dispatched, so the original key is still safe to pass.
        ownEvent_ = nullptr; editing_ = false; dispatching_ = false;
        pendingTimer_->stop(); pending_ = {};
        scheduleChanged();
        return false;
    }
    pending_.confirmation.insert(QStringLiteral("accepted_ns"), u64(dispatchNs));
    pending_.deadline = std::min(acceptanceLease, dispatchNs + 750000000ULL);
    QCoreApplication::sendEvent(before.target.object, &edit);
    ownEvent_ = nullptr;
    dispatching_ = false;
    ++revision_; last_.valid = false;
    editing_ = false;
    resolvePending(); // Unchanged native state stays pending, never rejected.
    scheduleChanged();
    return true;
}
bool QtClient::eventFilter(QObject *object, QEvent *event) {
    if (!connected_ || event == ownEvent_) return false;
    const auto *focus = hooks_.focusObject();
    if (object != focus && object != hooks_.focusWindow() &&
        object != ghost_.ghostTarget && object != ghost_.witness.target.window &&
        object != scopeTarget_.object && object != scopeTarget_.window &&
        (!pending_.active || object != pending_.before.target.object)) return false;
    switch (event->type()) {
    case QEvent::InputMethod: {
        const auto *input = static_cast<QInputMethodEvent *>(event);
        userPreedit_.insert(object, !input->preeditString().isEmpty());
        invalidate(true);
        break;
    }
    case QEvent::KeyPress: // Also covers application-synthesized native events.
    case QEvent::MouseButtonPress:
    case QEvent::Wheel:
    case QEvent::TouchBegin:
    case QEvent::ContextMenu:
    case QEvent::Drop:
        invalidate(true); break;
    case QEvent::FocusOut:
    case QEvent::Hide:
    case QEvent::WindowDeactivate:
        // Revoke before the old target's native focus-out handler can commit
        // preedit, even if Qt's focus pointer already names the next field.
        withdrawPolicy(); target(); invalidate(); break;
    case QEvent::DynamicPropertyChange:
    case QEvent::FontChange:
    case QEvent::StyleChange:
        invalidate(); break;
    case QEvent::Resize:
    case QEvent::Move:
        revoke(); scheduleChanged(); break;
    default: break;
    }
    return false;
}
} // namespace brotypist
