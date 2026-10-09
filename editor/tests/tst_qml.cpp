#include "backend.h"

#include <QImage>
#include <QPainter>
#include <QQmlContext>
#include <QQmlEngine>
#include <QTemporaryDir>
#include <QtQuickTest>

// A picture for the UI test and a folder for the fake CLI to write into.
class Setup : public QObject
{
    Q_OBJECT
public:
    Setup()
    {
        // The environment names the fake CLI for the tests; the product itself ignores it.
        if (qEnvironmentVariableIsSet("TELAMON_SCREENSHOT_BIN"))
            Backend::setCliPathForTests(qEnvironmentVariable("TELAMON_SCREENSHOT_BIN"));
        QImage img(400, 300, QImage::Format_RGB32);
        QPainter p(&img);
        p.fillRect(img.rect(), QColor(240, 240, 250));
        p.fillRect(QRect(40, 40, 200, 100), QColor(60, 90, 200));
        p.end();
        m_image = m_dir.filePath(QStringLiteral("input.png"));
        img.save(m_image);
        qputenv("FAKE_CLI_DIR", m_dir.filePath(QStringLiteral("out")).toUtf8());
        QDir(m_dir.path()).mkdir(QStringLiteral("out"));
    }

public slots:
    void qmlEngineAvailable(QQmlEngine *engine)
    {
        engine->rootContext()->setContextProperty(QStringLiteral("testImagePath"), m_image);
        engine->rootContext()->setContextProperty(QStringLiteral("fakeCliDir"), m_dir.filePath(QStringLiteral("out")));
    }

private:
    QTemporaryDir m_dir;
    QString m_image;
};

QUICK_TEST_MAIN_WITH_SETUP(editor, Setup)
#include "tst_qml.moc"
