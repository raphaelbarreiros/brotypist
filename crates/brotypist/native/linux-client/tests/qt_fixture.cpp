// Owned synthetic host. No adapter instantiation, fake IM hooks or private ABI access.
// The real patched Fcitx Qt6 platform input context must register with the broker.
#include <QApplication>
#include <QEvent>
#include <QFontMetricsF>
#include <QInputMethodEvent>
#include <QInputMethodQueryEvent>
#include <QKeyEvent>
#include <QLineEdit>
#include <QMouseEvent>
#include <QPointer>
#include <QScreen>
#include <QSocketNotifier>
#include <QTimer>
#include <QVBoxLayout>
#include <QWindow>
#include <cstdio>
#include <cstdlib>
#include <unistd.h>
#include <functional>
#include <algorithm>

class QueryLineEdit final : public QLineEdit {
public:
    using QLineEdit::QLineEdit;
    std::function<void(Qt::InputMethodQueries)> afterQuery;
protected:
    bool event(QEvent *event) override {
        const auto queries = event->type() == QEvent::InputMethodQuery ?
            static_cast<QInputMethodQueryEvent *>(event)->queries() : Qt::InputMethodQueries();
        const bool handled = QLineEdit::event(event);
        // Return the real query result, then let owned application code withdraw
        // the target synchronously. The callback may delete this widget.
        const auto callback = queries ? afterQuery : std::function<void(Qt::InputMethodQueries)>();
        if (callback) callback(queries);
        return handled;
    }
};

class Fixture final : public QObject {
public:
    QWidget host;
    QPointer<QueryLineEdit> first;
    QPointer<QLineEdit> second;
    bool deferPaint = false, queryOnPaint = false, queryDestroy = false, geometryQuerySeen = false;
    unsigned deferred = 0, commits = 0, queryCountdown = 0, queryInvalidations = 0;
    // Saturating widget-delivery observations after Fcitx forwarding, not raw
    // XTest metadata. Replay may reset timestamps or the auto-repeat flag.
    // Include modifier events: Shift+A ownership smoke reads each sequence
    // boundary so a leaked release cannot cancel out a later stolen release.
    unsigned keyPresses = 0, keyReleases = 0, zeroTimeKeys = 0, repeatPresses = 0;
    QString withdrawnText;
    int withdrawnCaret = -1, withdrawnSelection = -1;
    QByteArray input;
    Fixture() {
        host.setWindowTitle("BroTypist owned synthetic Qt fixture");
        host.resize(720, 220);
        auto *layout = new QVBoxLayout(&host);
        first = new QueryLineEdit(QStringLiteral("seed "), &host);
        installQueryHandler();
        second = new QLineEdit(QStringLiteral("other"), &host);
        layout->addWidget(first); layout->addWidget(second);
        qApp->installEventFilter(this);
        auto *reader = new QSocketNotifier(STDIN_FILENO, QSocketNotifier::Read, this);
        connect(reader, &QSocketNotifier::activated, this, [this] {
            char bytes[128]; const auto n = ::read(STDIN_FILENO, bytes, sizeof bytes);
            if (n <= 0) { qApp->quit(); return; }
            input.append(bytes, int(n));
            if (input.size() > 256) std::exit(3);
            int end;
            while ((end = input.indexOf('\n')) >= 0) {
                const auto command = input.left(end); input.remove(0, end + 1);
                execute(command);
            }
        });
        host.show(); host.activateWindow(); first->setFocus(); first->setCursorPosition(5);
    }
    void installQueryHandler() {
        first->afterQuery = [this](Qt::InputMethodQueries queries) {
            if (queryOnPaint || !queryCountdown) return;
            // Ignore unrelated platform cursor updates before the native
            // presentation geometry query. Keep the real widget's answers.
            if (queries == (Qt::ImCursorRectangle | Qt::ImFont)) {
                geometryQuerySeen = true;
                return;
            }
            if (queries != Qt::ImCursorRectangle || !geometryQuerySeen || --queryCountdown) return;
            ++queryInvalidations;
            // Freeze actual native state before destruction makes readback
            // impossible; never derive this witness from the offered edit.
            withdrawnText = first->text();
            withdrawnCaret = first->cursorPosition();
            withdrawnSelection = first->selectedText().size();
            if (queryDestroy) delete first.data();
            else host.hide();
        };
    }
    bool ownedPopup(QWindow *window) const {
        return window && window->transientParent() == host.windowHandle() &&
            (window->flags() & Qt::WindowType_Mask) == Qt::ToolTip;
    }
    bool eventFilter(QObject *object, QEvent *event) override {
        if ((object == first || object == second) && event->type() == QEvent::InputMethod &&
            !static_cast<QInputMethodEvent *>(event)->commitString().isEmpty()) ++commits;
        if (object == first && (event->type() == QEvent::KeyPress || event->type() == QEvent::KeyRelease)) {
            auto &count = event->type() == QEvent::KeyPress ? keyPresses : keyReleases;
            count = std::min(count + 1, 4096U);
            if (!static_cast<QKeyEvent *>(event)->timestamp()) zeroTimeKeys = std::min(zeroTimeKeys + 1, 4096U);
            if (event->type() == QEvent::KeyPress && static_cast<QKeyEvent *>(event)->isAutoRepeat())
                repeatPresses = std::min(repeatPresses + 1, 4096U);
        }
        if (deferPaint && ownedPopup(qobject_cast<QWindow *>(object)) &&
            (event->type() == QEvent::Expose || event->type() == QEvent::UpdateRequest)) {
            ++deferred;
            return true; // Delay real owner-loop painting, never forge exposure or a receipt.
        }
        if (!deferPaint && queryOnPaint && ownedPopup(qobject_cast<QWindow *>(object)) &&
            (event->type() == QEvent::Expose || event->type() == QEvent::UpdateRequest)) {
            queryOnPaint = false;
        }
        return QObject::eventFilter(object, event);
    }
    void state() {
        const auto a = first ? first->text().toUtf8().toHex() : QByteArray("destroyed");
        const auto b = second->text().toUtf8().toHex();
        unsigned popups = 0, exposed = 0, unexposed = 0;
        for (auto *window : QGuiApplication::topLevelWindows()) {
            if (!ownedPopup(window)) continue;
            if (window->isVisible()) ++popups;
            if (window->isExposed()) ++exposed;
            else ++unexposed;
        }
        std::printf("%s\t%d\t%s\t%u\t%u\t%u\t%u\t%u\t%d\t%u\t%d\t%llu\t%u\t%u\t%u\t%u\n", a.constData(),
            first ? first->cursorPosition() : -1, b.constData(), commits, deferred, popups,
            exposed, unexposed, first ? int(first->selectedText().size()) : -1, queryInvalidations,
            host.windowHandle() && host.windowHandle()->isExposed(), static_cast<unsigned long long>(host.winId()),
            keyPresses, keyReleases, zeroTimeKeys, repeatPresses);
        std::fflush(stdout);
    }
    void geometry() {
        if (!first || !host.windowHandle() || !host.windowHandle()->screen()) std::exit(5);
        const QFont font = first->inputMethodQuery(Qt::ImFont).value<QFont>();
        const QFontMetricsF metrics(font);
        // Independent native font measurement of the exact bounded fixture row.
        // No font/desktop resizing and no synthetic geometry overrides.
        const QString wide = QStringLiteral("2  ") + QString(160, QLatin1Char('W'));
        std::printf("geometry\t%.3f\t%d\t%.3f\t%.3f\n",
            metrics.horizontalAdvance(wide),
            host.windowHandle()->screen()->availableGeometry().width(),
            metrics.horizontalAdvance(QStringLiteral("1  first")),
            metrics.horizontalAdvance(QStringLiteral("2  third")));
        std::fflush(stdout);
    }
    void execute(const QByteArray &command) {
        if (command == "geometry") { geometry(); return; }
        if (command == "query-result") {
            std::printf("query-result\t%s\t%s\t%d\t%d\t%d\n",
                queryInvalidations == 1 ? (queryDestroy ? "query-destroy" : "query-hide") : "none",
                withdrawnText.toUtf8().toHex().constData(), withdrawnCaret, withdrawnSelection,
                host.isVisible());
            std::fflush(stdout);
            return;
        }
        if (command == "state") {}
        else if (command == "reset") {
            queryCountdown = 0; queryOnPaint = false; queryInvalidations = 0;
            withdrawnText.clear(); withdrawnCaret = withdrawnSelection = -1;
            if (!first) {
                first = new QueryLineEdit(&host); installQueryHandler();
                static_cast<QVBoxLayout *>(host.layout())->insertWidget(0, first);
            }
            first->setText(QStringLiteral("seed "));
            second->setText(QStringLiteral("other"));
            host.showNormal(); host.activateWindow(); first->setFocus();
            first->setCursorPosition(5); commits = 0;
            keyPresses = keyReleases = zeroTimeKeys = repeatPresses = 0;
        } else if ((command == "unicode-selected" || command == "unicode-collapsed") && first) {
            first->setText(QString::fromUtf8("A😀B")); first->setCursorPosition(3);
            if (command == "unicode-selected") first->setSelection(1, 2);
            commits = 0;
            keyPresses = keyReleases = zeroTimeKeys = repeatPresses = 0;
        } else if (command == "query-hide" || command == "query-destroy" ||
                   command == "query-paint-hide" || command == "query-paint-destroy" ||
                   command == "query-final-hide" || command == "query-final-destroy") {
            queryOnPaint = command.contains("paint") || command.contains("final");
            queryDestroy = command.endsWith("destroy");
            queryCountdown = command.contains("final") ? 2 : 1;
            queryInvalidations = 0;
            geometryQuerySeen = false;
        } else if (command == "move") host.move(host.pos() + QPoint(17, 13));
        else if (command == "minimize") host.showMinimized();
        else if (command == "focus2") second->setFocus();
        else if (command == "focus1" && first) first->setFocus();
        else if (command == "hide") host.hide();
        else if (command == "show") { host.show(); host.activateWindow(); if (first) first->setFocus(); }
        else if (command == "destroy") delete first.data();
        else if (command == "defer") { deferPaint = true; deferred = 0; }
        else if (command == "resume") {
            deferPaint = false;
            for (auto *window : QGuiApplication::topLevelWindows())
                if (ownedPopup(window)) window->requestUpdate();
        } else if (command == "hide-popup" || command == "choice2") {
            for (auto *window : QGuiApplication::topLevelWindows()) {
                if (!ownedPopup(window) || !window->isVisible()) continue;
                if (command == "hide-popup") window->hide();
                else {
                    // The scenario requires exactly two compact rows. Native mouse
                    // dispatch exercises the production row hit test, not Arm/accept internals.
                    const QPointF point(window->width() / 2.0, window->height() * 0.75);
                    QMouseEvent click(QEvent::MouseButtonRelease, point,
                        window->mapToGlobal(point.toPoint()), Qt::LeftButton, Qt::NoButton, Qt::NoModifier);
                    QCoreApplication::sendEvent(window, &click);
                }
                break;
            }
        } else std::exit(4);
        state();
    }
};
int main(int argc, char **argv) {
    if (geteuid() == 0 || qEnvironmentVariable("BROTYPIST_OWNED_CLIENT_SMOKE") != "1" ||
        qEnvironmentVariableIsEmpty("BROTYPIST_NATIVE_BUS") ||
        qEnvironmentVariable("QT_IM_MODULE") != "fcitx") return 2;
    // Owned-runner evidence only. Do not forward Qt messages, paths, input text,
    // or assertion expressions to stderr; Qt retains its normal fatal behavior.
    qInstallMessageHandler([](QtMsgType type, const QMessageLogContext &, const QString &message) {
        const char *label = "owned-smoke:qt-info\n";
        if (type == QtFatalMsg)
            label = message.startsWith(QLatin1String("ASSERT")) ?
                "owned-smoke:qt-assertion\n" : "owned-smoke:qt-fatal\n";
        else if (type == QtCriticalMsg) label = "owned-smoke:qt-critical\n";
        else if (type == QtWarningMsg) label = "owned-smoke:qt-warning\n";
        std::fputs(label, stderr);
        std::fflush(stderr);
    });
    QApplication app(argc, argv);
    if (QString::fromLatin1(qVersion()) != QStringLiteral("6.12.0")) return 2;
    Fixture fixture;
    return app.exec();
}
