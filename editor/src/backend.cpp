#include "backend.h"

#include "render.h"

#include <QBuffer>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QImageReader>
#include <QImageWriter>
#include <QPointer>
#include <QProcess>
#include <QProcessEnvironment>
#include <QSaveFile>
#include <QStandardPaths>
#include <QTimer>

#include <memory>

#include <cerrno>
#include <fcntl.h>
#include <poll.h>
#include <sys/stat.h>
#include <unistd.h>

using namespace Qt::StringLiterals;

namespace {

// The formats a screenshot can come in. Anything else (SVG, PDF, ...) is not decoded.
bool allowedFormat(const QByteArray &format)
{
    static const QList<QByteArray> ok = {"png", "jpeg", "jpg", "webp", "bmp", "gif"};
    return ok.contains(format.toLower());
}

// Plain, short, one line: what a helper printed, safe to show.
QString plainLine(const QByteArray &raw, int max = 200)
{
    QString s = QString::fromUtf8(raw.left(4096)).section(u'\n', 0, 0).trimmed();
    for (QChar &c : s) {
        if (c.unicode() < 0x20 || c.unicode() == 0x7f)
            c = u' ';
    }
    return s.left(max);
}

// A read-only view of the first `max` bytes of another device, so a file that
// grows while it is read (or a device that has no end) is never read past the cap.
class BoundedDevice : public QIODevice
{
public:
    BoundedDevice(QIODevice *inner, qint64 max)
        : m_inner(inner), m_max(max)
    {
        open(QIODevice::ReadOnly);
    }
    bool isSequential() const override { return m_inner->isSequential(); }
    qint64 size() const override { return qMin(m_inner->size(), m_max); }
    bool seek(qint64 pos) override
    {
        if (pos < 0 || pos > m_max || !m_inner->seek(pos))
            return false;
        return QIODevice::seek(pos);
    }

protected:
    qint64 readData(char *data, qint64 len) override
    {
        const qint64 left = m_max - m_inner->pos();
        if (left <= 0)
            return 0;
        return m_inner->read(data, qMin(len, left));
    }
    qint64 writeData(const char *, qint64) override { return -1; }

private:
    QIODevice *m_inner;
    qint64 m_max;
};

} // namespace

namespace shot {

QImage decodeImage(QIODevice *device, QString *error)
{
    QImageReader::setAllocationLimit(1280); // MiB: a 16384 x 16384 image is 1 GiB
    if (device->size() > Backend::kMaxInputBytes) {
        *error = QObject::tr("The file is too large.");
        return {};
    }
    BoundedDevice bounded(device, Backend::kMaxInputBytes);
    QImageReader reader(&bounded);
    reader.setAutoTransform(true);
    if (!reader.canRead() || !allowedFormat(reader.format())) {
        *error = QObject::tr("This is not an image the editor can open.");
        return {};
    }
    const QSize size = reader.size();
    if (!size.isValid() || size.isEmpty()) {
        *error = QObject::tr("The image has no readable size.");
        return {};
    }
    if (size.width() > Backend::kMaxSide || size.height() > Backend::kMaxSide
        || qint64(size.width()) * size.height() * 4 > Backend::kMaxPixelBytes) {
        *error = QObject::tr("The image is too large (%1 x %2 pixels; the limit is %3 pixels a side).")
                     .arg(size.width()).arg(size.height()).arg(Backend::kMaxSide);
        return {};
    }
    QImage image = reader.read();
    if (image.isNull()) {
        *error = QObject::tr("The image could not be read.");
        return {};
    }
    // One of two plain formats, whatever the file had.
    const QImage::Format want = image.hasAlphaChannel() ? QImage::Format_ARGB32 : QImage::Format_RGB32;
    if (image.format() != want)
        image.convertTo(want);
    return image;
}

QImage decodeFile(const QString &path, QString *error)
{
    // Opened once, and every check is made on the open descriptor, so nothing
    // can be swapped in between. O_NONBLOCK: opening a FIFO neither waits for
    // a writer nor blocks later (it is refused below, as is any device).
    // Symlinks are followed: people open linked files.
    const int fd = ::open(QFile::encodeName(path).constData(), O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) {
        *error = errno == ENOENT || errno == ENOTDIR ? QObject::tr("The file does not exist.")
                                                       : QObject::tr("The file could not be opened.");
        return {};
    }
    struct stat st;
    if (::fstat(fd, &st) != 0) {
        ::close(fd);
        *error = QObject::tr("The file could not be opened.");
        return {};
    }
    if (!S_ISREG(st.st_mode)) {
        ::close(fd);
        *error = QObject::tr("This is not a file.");
        return {};
    }
    if (st.st_size > Backend::kMaxInputBytes) {
        ::close(fd);
        *error = QObject::tr("The file is too large.");
        return {};
    }
    QFile file;
    if (!file.open(fd, QIODevice::ReadOnly, QFile::AutoCloseHandle)) {
        ::close(fd);
        *error = QObject::tr("The file could not be opened.");
        return {};
    }
    return decodeImage(&file, error);
}

} // namespace shot

Backend::Backend(QObject *parent)
    : QObject(parent)
{
}

Backend::~Backend()
{
    if (m_thread) {
        m_thread->requestInterruption();
        m_thread->wait();
    }
}

namespace {
QString &cliOverride()
{
    static QString path;
    return path;
}
} // namespace

QString Backend::cliPath()
{
    const QString &over = cliOverride();
    return over.isEmpty() ? u"/usr/bin/telamon-screenshot"_s : over;
}

void Backend::setCliPathForTests(const QString &path)
{
    cliOverride() = path;
}

QString Backend::baseName(const QString &path) const
{
    return QFileInfo(path).fileName();
}

void Backend::load(const QString &source)
{
    if (m_loading || source.isEmpty())
        return;
    m_loading = true;
    emit loadingChanged();

    const bool fromStdin = source == u"-"_s;
    const QString name = fromStdin ? QString() : QFileInfo(source).fileName();
    m_thread = QThread::create([this, source, fromStdin, name] {
        QString error;
        QImage image;
        if (fromStdin) {
            QByteArray bytes;
            bool ok = true;
            // poll() with a timeout, so closing the window never waits on a
            // pipe that nobody writes to.
            for (;;) {
                if (QThread::currentThread()->isInterruptionRequested()) {
                    ok = false;
                    break;
                }
                pollfd pfd{STDIN_FILENO, POLLIN, 0};
                const int r = ::poll(&pfd, 1, 100);
                if (r < 0) {
                    if (errno == EINTR)
                        continue;
                    ok = false;
                    error = tr("The image could not be read from the pipe.");
                    break;
                }
                if (r == 0)
                    continue;
                char buf[64 * 1024];
                const ssize_t n = ::read(STDIN_FILENO, buf, sizeof buf);
                if (n < 0) {
                    if (errno == EINTR || errno == EAGAIN)
                        continue;
                    ok = false;
                    error = tr("The image could not be read from the pipe.");
                    break;
                }
                if (n == 0)
                    break;
                if (bytes.size() + n > kMaxInputBytes) {
                    ok = false;
                    error = tr("The image is too large.");
                    break;
                }
                bytes.append(buf, n);
            }
            if (ok && bytes.isEmpty()) {
                ok = false;
                error = tr("No image was received.");
            }
            if (ok) {
                QBuffer buffer(&bytes);
                buffer.open(QIODevice::ReadOnly);
                image = shot::decodeImage(&buffer, &error);
            }
        } else {
            image = shot::decodeFile(source, &error);
        }
        QMetaObject::invokeMethod(this, [this, image, error, name] { finishLoad(image, error, name); }, Qt::QueuedConnection);
    });
    connect(m_thread, &QThread::finished, m_thread, &QObject::deleteLater);
    connect(m_thread, &QObject::destroyed, this, [this] { m_thread = nullptr; });
    m_thread->start();
}

void Backend::finishLoad(const QImage &image, const QString &error, const QString &name)
{
    m_loading = false;
    emit loadingChanged();
    if (image.isNull()) {
        emit loadFailed(error.isEmpty() ? tr("The image could not be opened.") : error);
        return;
    }
    m_image = image;
    m_sourceName = name;
    emit sourceNameChanged();
    emit imageChanged();
    emit loaded();
}

QByteArray Backend::exportPng(const QVariantList &items, const QRectF &crop) const
{
    const QImage out = shot::renderExport(m_image, shot::parseAnnotations(items), crop);
    return out.isNull() ? QByteArray() : shot::encodePng(out);
}

bool Backend::save(const QVariantList &items, const QRectF &crop)
{
    const QByteArray png = exportPng(items, crop);
    if (png.isEmpty()) {
        emit helperFinished(u"save"_s, false, tr("There is nothing to save."));
        return false;
    }
    return runHelper(u"save"_s, u"--save-png"_s, png);
}

bool Backend::copy(const QVariantList &items, const QRectF &crop)
{
    const QByteArray png = exportPng(items, crop);
    if (png.isEmpty()) {
        emit helperFinished(u"copy"_s, false, tr("There is nothing to copy."));
        return false;
    }
    return runHelper(u"copy"_s, u"--copy-png"_s, png);
}

bool Backend::runHelper(const QString &kind, const QString &arg, const QByteArray &png)
{
    // The program and an argument list: no shell, nothing from the image in either.
    auto *proc = new QProcess(this);
    proc->setProgram(cliPath());
    proc->setArguments({arg});
    proc->setProcessChannelMode(QProcess::SeparateChannels);

    ++m_helpers;
    emit helperBusyChanged();

    auto *timeout = new QTimer(proc);
    timeout->setSingleShot(true);
    timeout->setInterval(60000);
    connect(timeout, &QTimer::timeout, proc, &QProcess::kill);

    auto done = [this, proc, kind](bool ok, const QString &text) {
        --m_helpers;
        emit helperBusyChanged();
        proc->deleteLater();
        emit helperFinished(kind, ok, text);
    };
    auto finished = std::make_shared<bool>(false);

    connect(proc, &QProcess::started, this, [proc, png, timeout] {
        timeout->start();
        proc->write(png);
        proc->closeWriteChannel();
    });
    connect(proc, &QProcess::errorOccurred, this, [done, finished, proc](QProcess::ProcessError e) {
        if (e == QProcess::FailedToStart && !*finished) {
            *finished = true;
            done(false, tr("telamon-screenshot could not be started."));
        }
    });
    connect(proc, &QProcess::finished, this, [done, finished, proc](int code, QProcess::ExitStatus status) {
        if (*finished)
            return;
        *finished = true;
        const QString out = plainLine(proc->readAllStandardOutput());
        const QString err = plainLine(proc->readAllStandardError());
        if (status == QProcess::NormalExit && code == 0) {
            done(true, out);
        } else {
            done(false, err.isEmpty() ? tr("telamon-screenshot failed (exit status %1).").arg(code) : err);
        }
    });
    proc->start();
    return true;
}

QString Backend::suggestedSavePath() const
{
    QString dir = QStandardPaths::writableLocation(QStandardPaths::PicturesLocation);
    if (dir.isEmpty())
        dir = QDir::homePath();
    const QString shots = dir + u"/Screenshots"_s;
    if (QFileInfo(shots).isDir())
        dir = shots;
    QString stem = QFileInfo(m_sourceName).completeBaseName();
    if (stem.isEmpty())
        stem = u"Screenshot_"_s + QDateTime::currentDateTime().toString(u"yyyyMMdd_HHmmss"_s);
    else
        stem += u"_edited"_s;
    return dir + u'/' + stem + u".png"_s;
}

QVariantMap Backend::resolveOpenPath(const QString &typed) const
{
    QVariantMap out;
    QString p = typed.trimmed();
    if (p.isEmpty()) {
        out[u"error"_s] = tr("Enter the path of an image.");
        return out;
    }
    if (p == u"~"_s || p.startsWith(u"~/"_s))
        p = QDir::homePath() + p.mid(1);
    if (!QDir::isAbsolutePath(p)) {
        out[u"error"_s] = tr("Enter the full path of the file.");
        return out;
    }
    p = QDir::cleanPath(p);
    if (!QFileInfo(p).isFile()) {
        out[u"error"_s] = tr("There is no such file.");
        return out;
    }
    out[u"path"_s] = p;
    return out;
}

QVariantMap Backend::resolveSavePath(const QString &typed) const
{
    QVariantMap out;
    QString p = typed.trimmed();
    if (p.isEmpty()) {
        out[u"error"_s] = tr("Enter a file name.");
        return out;
    }
    if (p == u"~"_s || p.startsWith(u"~/"_s))
        p = QDir::homePath() + p.mid(1);
    if (!QDir::isAbsolutePath(p)) {
        out[u"error"_s] = tr("Enter the full path of the file.");
        return out;
    }
    p = QDir::cleanPath(p);
    if (QFileInfo(p).isDir()) {
        out[u"error"_s] = tr("This is a folder.");
        return out;
    }
    const QFileInfo info(p);
    const QString suffix = info.suffix().toLower();
    if (suffix.isEmpty())
        p += u".png"_s;
    else if (suffix != u"png"_s && suffix != u"jpg"_s && suffix != u"jpeg"_s) {
        out[u"error"_s] = tr("Use a .png or .jpg file name.");
        return out;
    }
    const QFileInfo fi(p);
    if (fi.isDir()) {
        out[u"error"_s] = tr("This is a folder.");
        return out;
    }
    if (!fi.dir().exists()) {
        out[u"error"_s] = tr("The folder does not exist.");
        return out;
    }
    out[u"path"_s] = p;
    out[u"exists"_s] = fi.exists();
    return out;
}

QString Backend::saveAs(const QString &path, const QVariantList &items, const QRectF &crop) const
{
    const QVariantMap r = resolveSavePath(path);
    if (r.contains(u"error"_s))
        return r.value(u"error"_s).toString();
    const QString target = r.value(u"path"_s).toString();
    const QImage out = shot::renderExport(m_image, shot::parseAnnotations(items), crop);
    if (out.isNull())
        return tr("There is nothing to save.");

    const QString suffix = QFileInfo(target).suffix().toLower();
    const QByteArray format = suffix == u"png"_s ? "png" : "jpeg";
    // Written to a temporary file next to the target and renamed over it: a
    // crash or a full disk never leaves half a file under the real name.
    const bool existed = QFileInfo::exists(target);
    QSaveFile file(target);
    file.setDirectWriteFallback(false);
    if (!file.open(QIODevice::WriteOnly))
        return tr("The file could not be written: %1").arg(file.errorString());
    // A new file is private, like the files of the CLI (the umask would make
    // it readable by others); a file that is replaced keeps its mode.
    if (!existed && !file.setPermissions(QFileDevice::ReadOwner | QFileDevice::WriteOwner)) {
        file.cancelWriting();
        return tr("The file could not be written: %1").arg(file.errorString());
    }
    QImageWriter writer(&file, format);
    if (format == "jpeg")
        writer.setQuality(92);
    const QImage toWrite = (format == "jpeg" && out.hasAlphaChannel()) ? out.convertedTo(QImage::Format_RGB32) : out;
    if (!writer.write(toWrite)) {
        file.cancelWriting();
        return tr("The image could not be encoded: %1").arg(writer.errorString());
    }
    if (!file.commit())
        return tr("The file could not be written: %1").arg(file.errorString());
    return {};
}

bool Backend::startCapture(const QString &mode, int delaySeconds)
{
    static const QStringList modes = {u"region"_s, u"full"_s, u"active-window"_s, u"window"_s, u"screen"_s};
    if (!modes.contains(mode))
        return false;
    QStringList args{u"--"_s + mode};
    const int delay = qBound(0, delaySeconds, 60);
    if (delay > 0)
        args << u"--delay"_s << QString::number(delay);
    args << u"--edit"_s;
    return QProcess::startDetached(cliPath(), args);
}
