// SPDX-License-Identifier: BSD-3-Clause
#pragma once

#include <QDBusConnection>
#include <QHash>
#include <QDBusContext>
#include <QDBusMessage>
#include <QFont>
#include <QPointer>
#include <QVariantMap>
#include <QWindow>
#include <functional>
#include <array>

class QKeyEvent;
class QTimer;
class QScreen;

namespace brotypist {

// All callbacks execute on the GUI thread. The upstream hook owns and reads
// private Fcitx state; this adapter never inspects an installed plugin ABI.
struct QtClientHooks {
    std::function<QObject *()> focusObject;
    std::function<QWindow *()> focusWindow;
    std::function<QString()> composition; // idle, active, or unknown
};

class ChoiceWindow;
class QtClient final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.brotypist.Client1")
public:
    explicit QtClient(QtClientHooks hooks, QObject *parent);
    ~QtClient() override;
    bool key(const QKeyEvent &event);
    void invalidate(bool ordinaryInput = false);
    void nativeChanged();

public Q_SLOTS:
    QVariantMap Metadata();
    QVariantMap Capture(qulonglong policy_epoch);
    QVariantMap Present(const QVariantMap &offer);
    bool Arm(const QVariantMap &acceptance);
    void Clear(qulonglong capability_id);
    void Configure(const QVariantMap &policy);

protected:
    bool eventFilter(QObject *object, QEvent *event) override;

private:
    friend class ChoiceWindow;
    struct Target {
        QPointer<QObject> object;
        QPointer<QWindow> window;
        QString field, windowToken, document;
        bool focused = false, protectedText = true, editable = false;
        QString purpose = QStringLiteral("unknown"), composition;
    };
    struct State {
        Target target;
        QVariantMap snapshot;
        QString text;
        int caret = -1, anchor = -1;
        bool valid = false;
    };
    struct Presentation {
        QVariantMap offer;
        State witness;
        QPointer<QObject> ghostTarget;
        QPointer<ChoiceWindow> popup;
        QRectF cursor, clip;
        QRectF queryCursor;
        QRect targetGeometry, popupGeometry, availableGeometry;
        QPointer<QScreen> screen;
        QFont font;
        QString shownText, word, all;
        QStringList shownValues;
        qulonglong identity = 0, capability = 0, lease = 0;
        bool armed = false;
    };
    struct PendingPresentation {
        QDBusMessage request;
        QString owner;
        qulonglong registration = 0, clientId = 0;
        bool active = false;
    };
    struct PendingEdit {
        QVariantMap confirmation;
        State before, finalAfter;
        QString expected, owner;
        int caret = -1;
        qulonglong deadline = 0, registration = 0, policyEpoch = 0, clientId = 0;
        bool active = false, finalSent = false;
    };
    bool authorized() const;
    bool policyLive() const;
    bool policyValid() const;
    void registerClient();
    void adoptPolicy(const QVariantMap &policy);
    Q_SLOT void transportLost();
    void event(QVariantMap value);
    bool sendAction(const QString &action);
    void changed();
    void scheduleChanged();
    Target target();
    State capture(bool pendingReadback = false);
    void resolvePending();
    void finishPending(const QString &outcome, const State &after);
    void withdrawPolicy();
    void observeScope(const Target &target);
    QString scopeToken(const QString &field, qlonglong origin);
    bool ghostGeometry(const State &state, const QString &text, QRectF *viewport = nullptr) const;
    bool choiceGeometry(const Presentation &presentation, const State &state) const;
    bool currentPresentation(const Presentation &witness, const State &state) const;
    void completePresentation(ChoiceWindow *window);
    bool replyPresentation(const QVariantMap &result);
    bool matches(const State &state, const QVariantMap &offer) const;
    void revoke();
    void revoke(Presentation &presentation);
    void scheduleExpiry();
    Presentation *presentation(qulonglong capability);
    bool live(const Presentation &presentation, const State &state) const;
    bool accept(Presentation &presentation, const QString &action, int index = -1);
    QString token(QObject *object);
    QString shortcut(const QString &action) const;
    bool feature(const QString &mode) const;
    bool keyPress(const QKeyEvent &event);

    QtClientHooks hooks_;
    QDBusConnection bus_;
    QString busName_, owner_, path_;
    qulonglong clientId_ = 0, registration_ = 0, nextToken_ = 0;
    qulonglong revision_ = 1, userRevision_ = 1, notificationRevision_ = 0;
    qulonglong nextPresentation_ = 0, nextCapability_ = 0, nextAcceptance_ = 0;
    QVariantMap policy_;
    State last_;
    Presentation ghost_, chooser_;
    Target observedTarget_, scopeTarget_;
    PendingEdit pending_;
    PendingPresentation pendingPresentation_;
    qulonglong focusGeneration_ = 1;
    bool scopeKnown_ = false, resolvingPending_ = false, finalizingPending_ = false;
    QHash<QString, QString> scopeTokens_;
    QHash<QObject *, QString> tokens_;
    QHash<QObject *, bool> userPreedit_;
    const QEvent *ownEvent_ = nullptr;
    struct OwnedKey {
        int key = 0;
        quint32 scan = 0, symbol = 0;
    };
    std::array<OwnedKey, 16> ownedKeys_{};
    QPointer<QObject> keyTarget_;
    QPointer<QWindow> keyWindow_;
    QTimer *expiry_, *policyExpiry_, *pendingTimer_;
    bool dispatching_ = false, editing_ = false, scheduled_ = false, querying_ = false, metadataQuery_ = false;
    bool connected_ = false;
    int pendingEvents_ = 0;
};
} // namespace brotypist
