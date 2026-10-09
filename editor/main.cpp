// telamon-screenshot-editor [FILE | -]
//
// The annotation editor of Telamon Screenshot: the one window of the product,
// a separate executable so the snip path of telamon-screenshot stays free of
// a toolkit. FILE is an image to open, "-" reads the image from stdin, no
// argument opens the empty state. Several instances may run at once.
#include "harden.h"

#include <QCommandLineParser>
#include <QApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QSGRendererInterface>

#ifdef TELAMON_EDITOR_TEST_HOOKS
#include "backend.h"

#include <QDateTime>
#include <QFile>
#include <QJSEngine>
#include <QQmlContext>
#include <QTimer>
#include <cstdio>
#include <memory>
#endif

using namespace Qt::StringLiterals;

int main(int argc, char *argv[])
{
    // Before anything holds a pixel: no core dump of a process with the screen in it.
    shot::hardenProcess();

#ifdef TELAMON_EDITOR_TEST_HOOKS
    // Only the test build lets the environment choose the CLI (the shipped
    // editor always runs /usr/bin/telamon-screenshot).
    if (qEnvironmentVariableIsSet("TELAMON_SCREENSHOT_BIN"))
        Backend::setCliPathForTests(qEnvironmentVariable("TELAMON_SCREENSHOT_BIN"));
#endif

    // Draw on the CPU like the other Telamon apps: for a window of an image
    // and a few shapes the GPU path costs memory and start-up time for
    // nothing. QT_QUICK_BACKEND overrides.
    if (qEnvironmentVariableIsEmpty("QT_QUICK_BACKEND"))
        QQuickWindow::setGraphicsApi(QSGRendererInterface::Software);

    // QApplication, not QGuiApplication: the Plasma style (org.kde.desktop) draws with QStyle, which
    // wants it (the file chooser fallback and any default control use it). Widgets is loaded by the
    // style anyway.
    QApplication app(argc, argv);
    QApplication::setApplicationName(u"Telamon Screenshot"_s);
    QApplication::setApplicationDisplayName(u"Telamon Screenshot"_s);
    QApplication::setOrganizationDomain(u"telamon.eterneon.net"_s);
    QApplication::setApplicationVersion(QStringLiteral(TELAMON_EDITOR_VERSION));
    QApplication::setDesktopFileName(u"net.eterneon.telamon.screenshot.editor"_s);
    QApplication::setWindowIcon(QIcon::fromTheme(u"accessories-screenshot"_s));

    // The Plasma style: Kirigami takes the colours and fonts of the desktop
    // (kdeglobals) from it, and Telamon.Ui follows Kirigami.
    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE"))
        QQuickStyle::setStyle(u"org.kde.desktop"_s);

    QCommandLineParser parser;
    parser.setApplicationDescription(u"Annotate a screenshot. FILE is an image to open, - reads the image from stdin."_s);
    parser.addHelpOption();
    parser.addVersionOption();
    parser.addPositionalArgument(u"file"_s, u"Image to open, or - for stdin."_s, u"[FILE | -]"_s);
#ifdef TELAMON_EDITOR_TEST_HOOKS
    // Only in the test build: grab the first stable frame to a PNG, run a
    // script on the window first, print the time of the first frame.
    const QCommandLineOption shotOpt(u"screenshot"_s, u"Grab the window to FILE and quit."_s, u"file"_s);
    const QCommandLineOption scenarioOpt(u"scenario"_s, u"JS file run with the window as `win`."_s, u"file"_s);
    const QCommandLineOption firstFrameOpt(u"first-frame"_s, u"Print the epoch time (ms) of the first frame, then RSS and peak RSS (kB), and quit."_s);
    const QCommandLineOption waitOpt(u"wait"_s, u"Milliseconds to wait before the grab (default 700)."_s, u"ms"_s);
    parser.addOptions({shotOpt, scenarioOpt, firstFrameOpt, waitOpt});
#endif
    parser.process(app);

    const QStringList positional = parser.positionalArguments();
    if (positional.size() > 1) {
        fprintf(stderr, "telamon-screenshot-editor: at most one image\n");
        return 2;
    }

    QQmlApplicationEngine engine;
    engine.setInitialProperties({{u"startSource"_s, positional.isEmpty() ? QString() : positional.first()}});
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreationFailed, &app, [] { QCoreApplication::exit(3); },
                     Qt::QueuedConnection);
    engine.loadFromModule("net.eterneon.telamon.screenshoteditor", "Main");
    if (engine.rootObjects().isEmpty())
        return 3;

#ifdef TELAMON_EDITOR_TEST_HOOKS
    auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (window && parser.isSet(firstFrameOpt)) {
        auto conn = std::make_shared<QMetaObject::Connection>();
        *conn = QObject::connect(window, &QQuickWindow::frameSwapped, &app, [conn, window] {
            QObject::disconnect(*conn);
            const qint64 t = QDateTime::currentMSecsSinceEpoch();
            // The frame of the image, not only the empty window: wait for the load, then report the memory a moment later.
            QTimer::singleShot(600, window, [t] {
                long rss = 0, hwm = 0;
                QFile f(u"/proc/self/status"_s);
                if (f.open(QIODevice::ReadOnly)) {
                    for (const QByteArray &l : f.readAll().split('\n')) {
                        if (l.startsWith("VmRSS:"))
                            rss = l.mid(6).trimmed().split(' ').first().toLong();
                        else if (l.startsWith("VmHWM:"))
                            hwm = l.mid(6).trimmed().split(' ').first().toLong();
                    }
                }
                printf("%lld %ld %ld\n", static_cast<long long>(t), rss, hwm);
                fflush(stdout);
                QCoreApplication::quit();
            });
        });
    }
    if (window && parser.isSet(shotOpt)) {
        const QString out = parser.value(shotOpt);
        const int wait = parser.isSet(waitOpt) ? parser.value(waitOpt).toInt() : 700;
        QTimer::singleShot(wait, &app, [&engine, window, out, &parser, &scenarioOpt, wait] {
            if (parser.isSet(scenarioOpt)) {
                QFile f(parser.value(scenarioOpt));
                if (f.open(QIODevice::ReadOnly)) {
                    QJSValue fn = engine.evaluate(QString::fromUtf8(f.readAll()), parser.value(scenarioOpt));
                    if (fn.isError()) {
                        fprintf(stderr, "scenario: %s\n", qPrintable(fn.toString()));
                        QCoreApplication::exit(4);
                        return;
                    }
                    QJSValue r = fn.call({engine.newQObject(window)});
                    if (r.isError()) {
                        fprintf(stderr, "scenario: %s (line %d)\n", qPrintable(r.toString()), r.property(u"lineNumber"_s).toInt());
                        QCoreApplication::exit(4);
                        return;
                    }
                }
            }
            QTimer::singleShot(wait, window, [window, out] {
                const QImage img = window->grabWindow();
                const bool ok = img.save(out);
                if (!ok)
                    fprintf(stderr, "could not write %s\n", qPrintable(out));
                QCoreApplication::exit(ok ? 0 : 5);
            });
        });
    }
#endif
    return app.exec();
}
