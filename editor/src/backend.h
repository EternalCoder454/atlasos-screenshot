// Everything the QML side cannot do itself: reading the image (a file or
// stdin) without trusting it, rendering the export, writing a file
// atomically, and talking to the telamon-screenshot CLI by argv.
#pragma once

#include <QImage>
#include <QObject>
#include <QRectF>
#include <QSize>
#include <QString>
#include <QThread>
#include <QVariantList>
#include <QVariantMap>
#include <qqmlintegration.h>

class QProcess;

class Backend : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QImage image READ image NOTIFY imageChanged FINAL)
    Q_PROPERTY(QSize imageSize READ imageSize NOTIFY imageChanged FINAL)
    Q_PROPERTY(bool loading READ loading NOTIFY loadingChanged FINAL)
    // True while a Save or Copy is with the CLI.
    Q_PROPERTY(bool helperBusy READ helperBusy NOTIFY helperBusyChanged FINAL)
    // The base name of the file that was opened, or empty (plain text).
    Q_PROPERTY(QString sourceName READ sourceName NOTIFY sourceNameChanged FINAL)

public:
    // Limits for what is decoded (the image is untrusted).
    static constexpr int kMaxSide = 16384;
    static constexpr qint64 kMaxPixelBytes = qint64(1) << 30; // 1 GiB of 32-bit pixels
    static constexpr qint64 kMaxInputBytes = qint64(256) << 20; // 256 MiB of file or stdin

    explicit Backend(QObject *parent = nullptr);
    ~Backend() override;

    QImage image() const { return m_image; }
    QSize imageSize() const { return m_image.size(); }
    bool loading() const { return m_loading; }
    bool helperBusy() const { return m_helpers > 0; }
    QString sourceName() const { return m_sourceName; }

    // `source` is a path, or "-" for stdin. Decodes on a worker thread and
    // emits loaded() or loadFailed().
    Q_INVOKABLE void load(const QString &source);

    // The image with the annotations, cropped, as PNG bytes. Empty when there
    // is nothing to export.
    Q_INVOKABLE QByteArray exportPng(const QVariantList &items, const QRectF &crop) const;

    // Save and Copy go through the CLI ("telamon-screenshot --save-png" and
    // "--copy-png", the PNG on stdin). The answer is helperFinished().
    Q_INVOKABLE bool save(const QVariantList &items, const QRectF &crop);
    Q_INVOKABLE bool copy(const QVariantList &items, const QRectF &crop);

    // Save As. resolveSavePath() works out the final path and says whether it
    // exists; saveAs() writes it atomically (QSaveFile) and returns an error
    // text, empty when it worked.
    Q_INVOKABLE QVariantMap resolveSavePath(const QString &typed) const;
    Q_INVOKABLE QString saveAs(const QString &path, const QVariantList &items, const QRectF &crop) const;
    Q_INVOKABLE QString suggestedSavePath() const;
    // The same for Open Image: { path } or { error }.
    Q_INVOKABLE QVariantMap resolveOpenPath(const QString &typed) const;

    // Starts a new capture with the CLI, detached: mode is one of region, full,
    // active-window, window, screen. The CLI opens a new editor with the result.
    Q_INVOKABLE bool startCapture(const QString &mode, int delaySeconds);

    Q_INVOKABLE QString baseName(const QString &path) const;

    // The CLI the helpers run: $TELAMON_SCREENSHOT_BIN (for tests), else /usr/bin/telamon-screenshot.
    static QString cliPath();

signals:
    void imageChanged();
    void loadingChanged();
    void helperBusyChanged();
    void sourceNameChanged();
    void loaded();
    void loadFailed(const QString &message);
    // kind is "save" or "copy"; text is the saved path or the reason it failed.
    void helperFinished(const QString &kind, bool ok, const QString &text);

private:
    bool runHelper(const QString &kind, const QString &arg, const QByteArray &png);
    void finishLoad(const QImage &image, const QString &error, const QString &name);

    QImage m_image;
    bool m_loading = false;
    int m_helpers = 0;
    QString m_sourceName;
    QThread *m_thread = nullptr;
};

namespace shot {
// Decode untrusted bytes or a file, with the size caps. On failure the image
// is null and `error` says why (plain text, short).
QImage decodeImage(QIODevice *device, QString *error);
QImage decodeFile(const QString &path, QString *error);
} // namespace shot
