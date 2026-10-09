// Export, decoding limits, atomic save and the CLI helpers (against the fake CLI).
#include "backend.h"
#include "harden.h"
#include "render.h"

#include <QBuffer>
#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QImage>
#include <QImageWriter>
#include <QRandomGenerator>
#include <QRegularExpression>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

#include <fcntl.h>
#include <future>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

using namespace Qt::StringLiterals;

namespace {

QImage randomImage(int w, int h, QImage::Format format)
{
    QImage img(w, h, format);
    auto *rng = QRandomGenerator::global();
    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            QRgb c = rng->generate();
            if (!img.hasAlphaChannel())
                c |= 0xff000000;
            img.setPixel(x, y, c);
        }
    }
    return img;
}

QVariantMap redact(qreal x, qreal y, qreal w, qreal h, const QString &color)
{
    return {{u"type"_s, u"redact"_s}, {u"x"_s, x}, {u"y"_s, y}, {u"w"_s, w}, {u"h"_s, h}, {u"color"_s, color}, {u"size"_s, 4}};
}

bool samePixels(const QImage &a, const QImage &b)
{
    if (a.size() != b.size())
        return false;
    for (int y = 0; y < a.height(); ++y)
        for (int x = 0; x < a.width(); ++x)
            if (a.pixel(x, y) != b.pixel(x, y))
                return false;
    return true;
}

QByteArray pngBytes(const QImage &img)
{
    QByteArray b;
    QBuffer buf(&b);
    buf.open(QIODevice::WriteOnly);
    img.save(&buf, "PNG");
    return b;
}

// A PNG of a few dozen bytes (valid signature, IHDR, an empty IDAT, IEND)
// whose header claims w x h pixels: a pixel bomb.
QByteArray pngClaiming(quint32 w, quint32 h)
{
    auto be32 = [](quint32 v) {
        QByteArray b(4, 0);
        for (int i = 0; i < 4; ++i)
            b[i] = char(v >> (24 - 8 * i));
        return b;
    };
    auto crc32 = [](const QByteArray &data) {
        quint32 c = 0xffffffffu;
        for (uchar byte : data) {
            c ^= byte;
            for (int k = 0; k < 8; ++k)
                c = (c & 1) ? (c >> 1) ^ 0xedb88320u : c >> 1;
        }
        return ~c;
    };
    auto chunk = [&](const char *type, const QByteArray &data) {
        const QByteArray td = QByteArray(type, 4) + data;
        return be32(data.size()) + td + be32(crc32(td));
    };
    QByteArray ihdr = be32(w) + be32(h);
    ihdr.append(char(8)).append(char(2)).append(char(0)).append(char(0)).append(char(0)); // 8 bit RGB
    const QByteArray emptyZlib = QByteArray::fromHex("789c030000000001");
    return QByteArray("\x89PNG\r\n\x1a\n", 8) + chunk("IHDR", ihdr) + chunk("IDAT", emptyZlib) + chunk("IEND", {});
}

bool writeFile(const QString &path, const QByteArray &bytes)
{
    QFile f(path);
    return f.open(QIODevice::WriteOnly) && f.write(bytes) == bytes.size();
}

bool waitFor(const std::function<bool()> &cond, int ms = 5000)
{
    QElapsedTimer t;
    t.start();
    while (!cond() && t.elapsed() < ms)
        QTest::qWait(10);
    return cond();
}

} // namespace

class TstBackend : public QObject
{
    Q_OBJECT

private:
    QByteArray m_cli;

private slots:
    // The environment names the fake CLI for the tests; the product ignores
    // it and only a test passes it on, through the seam.
    void initTestCase() { m_cli = qgetenv("TELAMON_SCREENSHOT_BIN"); }
    void init() { Backend::setCliPathForTests(QString::fromLocal8Bit(m_cli)); }
    void cleanup() { Backend::setCliPathForTests({}); }

    void theEnvironmentDoesNotChooseTheCli()
    {
        // A Backend that did not call the seam runs the installed CLI, whatever the environment says.
        Backend::setCliPathForTests({});
        const QByteArray old = qgetenv("TELAMON_SCREENSHOT_BIN");
        qputenv("TELAMON_SCREENSHOT_BIN", "/tmp/evil-telamon-screenshot");
        Backend be;
        QCOMPARE(Backend::cliPath(), u"/usr/bin/telamon-screenshot"_s);
        // And the seam still works for a test.
        Backend::setCliPathForTests(u"/some/fake"_s);
        QCOMPARE(Backend::cliPath(), u"/some/fake"_s);
        Backend::setCliPathForTests({});
        QCOMPARE(Backend::cliPath(), u"/usr/bin/telamon-screenshot"_s);
        qputenv("TELAMON_SCREENSHOT_BIN", old);
    }

    void coreDumpsAreOff()
    {
        QVERIFY(shot::hardenProcess());
        rlimit lim{1, 1};
        QCOMPARE(::getrlimit(RLIMIT_CORE, &lim), 0);
        QCOMPARE(quint64(lim.rlim_cur), quint64(0));
        QCOMPARE(quint64(lim.rlim_max), quint64(0));
    }

    void exportWithoutAnnotationsIsTheInput_data()
    {
        QTest::addColumn<int>("format");
        QTest::newRow("rgb") << int(QImage::Format_RGB32);
        QTest::newRow("argb") << int(QImage::Format_ARGB32);
    }
    void exportWithoutAnnotationsIsTheInput()
    {
        QFETCH(int, format);
        const QImage in = randomImage(97, 61, QImage::Format(format));
        const QImage out = shot::renderExport(in, {}, QRectF());
        QVERIFY(samePixels(in, out));
        // And through PNG and back, as the helpers get it.
        const QByteArray png = shot::encodePng(out);
        QImage back;
        QVERIFY(back.loadFromData(png, "PNG"));
        QVERIFY(samePixels(in, back));
    }

    void exportCropHasTheCropSize()
    {
        const QImage in = randomImage(120, 80, QImage::Format_RGB32);
        const QImage out = shot::renderExport(in, {}, QRectF(10, 20, 50, 30));
        QCOMPARE(out.size(), QSize(50, 30));
        QCOMPARE(out.pixel(0, 0), in.pixel(10, 20));
        QCOMPARE(out.pixel(49, 29), in.pixel(59, 49));
        // A crop beyond the image is cut to it; one outside it gives nothing.
        QCOMPARE(shot::renderExport(in, {}, QRectF(100, 60, 500, 500)).size(), QSize(20, 20));
        QVERIFY(shot::renderExport(in, {}, QRectF(500, 500, 10, 10)).isNull());
    }

    void redactIsExactlyTheFillAtFullAlpha_data()
    {
        QTest::addColumn<int>("format");
        QTest::addColumn<QString>("color");
        QTest::newRow("rgb black") << int(QImage::Format_RGB32) << u"#000000"_s;
        QTest::newRow("rgb odd") << int(QImage::Format_RGB32) << u"#1a2b3c"_s;
        QTest::newRow("argb odd") << int(QImage::Format_ARGB32) << u"#1a2b3c"_s;
        // A fill with alpha in the item is still opaque in the output.
        QTest::newRow("argb transparent fill") << int(QImage::Format_ARGB32) << u"#201a2b3c"_s;
    }
    void redactIsExactlyTheFillAtFullAlpha()
    {
        QFETCH(int, format);
        QFETCH(QString, color);
        QImage in = randomImage(100, 70, QImage::Format(format));
        if (format == QImage::Format_ARGB32) {
            // Part of the image is fully transparent, part half.
            for (int x = 0; x < 100; ++x) {
                in.setPixelColor(x, 10, QColor(255, 255, 255, 0));
                in.setPixelColor(x, 11, QColor(10, 20, 30, 128));
            }
        }
        // Fractional edges are rounded outwards to whole pixels.
        const QList<shot::Annotation> items = shot::parseAnnotations({redact(10.4, 9.2, 30.3, 5.5, color)});
        QCOMPARE(items.size(), 1);
        const QImage out = shot::renderExport(in, items, QRectF());
        QCOMPARE(out.size(), in.size());
        const QRect box(QPoint(10, 9), QPoint(40, 14)); // floor(10.4) .. ceil(40.7)-1, floor(9.2) .. ceil(14.7)-1
        QColor want(color);
        const QRgb fill = qRgba(want.red(), want.green(), want.blue(), 255);
        int inside = 0;
        for (int y = 0; y < in.height(); ++y) {
            for (int x = 0; x < in.width(); ++x) {
                if (box.contains(x, y)) {
                    QCOMPARE(out.pixel(x, y), fill);
                    ++inside;
                } else {
                    QCOMPARE(out.pixel(x, y), in.pixel(x, y));
                }
            }
        }
        QCOMPARE(inside, box.width() * box.height());
    }

    void redactSurvivesACropAndPng()
    {
        const QImage in = randomImage(100, 70, QImage::Format_RGB32);
        const auto items = shot::parseAnnotations({redact(20, 20, 20, 20, u"#000000"_s)});
        const QImage out = shot::renderExport(in, items, QRectF(15, 15, 30, 30));
        QCOMPARE(out.size(), QSize(30, 30));
        QImage back;
        QVERIFY(back.loadFromData(shot::encodePng(out), "PNG"));
        for (int y = 5; y < 25; ++y)
            for (int x = 5; x < 25; ++x)
                QCOMPARE(back.pixel(x, y), 0xff000000u);
        QCOMPARE(back.pixel(4, 4), in.pixel(19, 19));
    }

    void annotationsOutsideTheCropAreCut()
    {
        const QImage in = randomImage(100, 70, QImage::Format_RGB32);
        const auto items = shot::parseAnnotations({redact(0, 0, 100, 70, u"#ff0000"_s)});
        const QImage out = shot::renderExport(in, items, QRectF(10, 10, 20, 20));
        QCOMPARE(out.size(), QSize(20, 20));
        QCOMPARE(out.pixel(0, 0), 0xffff0000u);
        QCOMPARE(out.pixel(19, 19), 0xffff0000u);
    }

    void everyToolPaintsSomething()
    {
        QImage in(200, 120, QImage::Format_RGB32);
        in.fill(Qt::white);
        const QVariantList items = {
            QVariantMap{{u"type"_s, u"arrow"_s}, {u"x1"_s, 10}, {u"y1"_s, 10}, {u"x2"_s, 90}, {u"y2"_s, 40}, {u"color"_s, u"#e5484d"_s}, {u"size"_s, 5}},
            QVariantMap{{u"type"_s, u"rect"_s}, {u"x"_s, 100}, {u"y"_s, 10}, {u"w"_s, 60}, {u"h"_s, 40}, {u"color"_s, u"#3e63dd"_s}, {u"size"_s, 4}},
            QVariantMap{{u"type"_s, u"pen"_s}, {u"pts"_s, QVariantList{10, 80, 30, 90, 50, 70, 70, 95}}, {u"color"_s, u"#30a46c"_s}, {u"size"_s, 4}},
            QVariantMap{{u"type"_s, u"highlighter"_s}, {u"pts"_s, QVariantList{100, 80, 190, 80}}, {u"color"_s, u"#ffd60a"_s}, {u"size"_s, 6}},
            QVariantMap{{u"type"_s, u"text"_s}, {u"x"_s, 100}, {u"y"_s, 55}, {u"text"_s, u"Hi"_s}, {u"color"_s, u"#000000"_s}, {u"size"_s, 20}},
            QVariantMap{{u"type"_s, u"marker"_s}, {u"x"_s, 20}, {u"y"_s, 60}, {u"number"_s, 3}, {u"color"_s, u"#e5484d"_s}, {u"size"_s, 24}},
        };
        const auto parsed = shot::parseAnnotations(items);
        QCOMPARE(parsed.size(), items.size());
        for (int i = 0; i < parsed.size(); ++i) {
            const QImage out = shot::renderExport(in, {parsed[i]}, QRectF());
            QVERIFY2(!samePixels(in, out), qPrintable(u"item %1 drew nothing"_s.arg(i)));
        }
    }

    void markupInTextIsDrawnAsText()
    {
        QImage in(300, 60, QImage::Format_RGB32);
        in.fill(Qt::white);
        const auto a = shot::parseAnnotations({QVariantMap{{u"type"_s, u"text"_s}, {u"x"_s, 5}, {u"y"_s, 5}, {u"text"_s, u"<b>bold</b> &amp;"_s}, {u"color"_s, u"#000000"_s}, {u"size"_s, 24}}});
        QCOMPARE(a.size(), 1);
        QCOMPARE(a[0].text, u"<b>bold</b> &amp;"_s);
    }

    void badItemsAreDropped()
    {
        QVariantList items = {
            QVariantMap{{u"type"_s, u"nope"_s}},
            QVariantMap{{u"type"_s, u"arrow"_s}, {u"x1"_s, 1}},
            QVariantMap{{u"type"_s, u"rect"_s}, {u"x"_s, 1e300}, {u"y"_s, 0}, {u"w"_s, 1}, {u"h"_s, 1}},
            QVariantMap{{u"type"_s, u"rect"_s}, {u"x"_s, qQNaN()}, {u"y"_s, 0}, {u"w"_s, 1}, {u"h"_s, 1}},
            QVariantMap{{u"type"_s, u"text"_s}, {u"x"_s, 1}, {u"y"_s, 1}, {u"text"_s, u""_s}},
            QVariantMap{{u"type"_s, u"pen"_s}, {u"pts"_s, QVariantList{1}}},
            QVariantMap{{u"type"_s, u"rect"_s}, {u"x"_s, 1}, {u"y"_s, 1}, {u"w"_s, 5}, {u"h"_s, 5}, {u"size"_s, 1e9}},
        };
        const auto parsed = shot::parseAnnotations(items);
        QCOMPARE(parsed.size(), 1);
        QVERIFY(parsed[0].size <= 4000);
        // A long text is cut.
        const auto t = shot::parseAnnotations({QVariantMap{{u"type"_s, u"text"_s}, {u"x"_s, 1}, {u"y"_s, 1}, {u"text"_s, QString(100000, u'x')}}});
        QCOMPARE(t.size(), 1);
        QCOMPARE(t[0].text.size(), shot::kMaxTextLength);
    }

    void decodeRefusesWhatItShould()
    {
        QString err;
        // Fine.
        QBuffer ok;
        QByteArray b = pngBytes(randomImage(30, 20, QImage::Format_RGB32));
        ok.setData(b);
        ok.open(QIODevice::ReadOnly);
        QCOMPARE(shot::decodeImage(&ok, &err).size(), QSize(30, 20));

        // Too wide: a small file (it compresses to nothing) that would be 20000 wide.
        QImage wide(20000, 1, QImage::Format_RGB32);
        wide.fill(Qt::black);
        QByteArray wb = pngBytes(wide);
        QVERIFY(wb.size() < 100000);
        QBuffer wbuf(&wb);
        wbuf.open(QIODevice::ReadOnly);
        err.clear();
        QVERIFY(shot::decodeImage(&wbuf, &err).isNull());
        QVERIFY(!err.isEmpty());

        // Garbage, text, and an SVG (a format that is not decoded).
        for (const QByteArray &junk : {QByteArray("not an image at all"), QByteArray(), QByteArray("<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10'/></svg>")}) {
            QByteArray copy = junk;
            QBuffer jb(&copy);
            jb.open(QIODevice::ReadOnly);
            err.clear();
            QVERIFY(shot::decodeImage(&jb, &err).isNull());
            QVERIFY(!err.isEmpty());
        }

        // A truncated PNG.
        QByteArray cut = b.left(b.size() / 2);
        QBuffer cb(&cut);
        cb.open(QIODevice::ReadOnly);
        QVERIFY(shot::decodeImage(&cb, &err).isNull());

        // Files: missing, a folder.
        QVERIFY(shot::decodeFile(u"/nonexistent/x.png"_s, &err).isNull());
        QVERIFY(shot::decodeFile(u"/tmp"_s, &err).isNull());
    }

    void pixelBombsAreRefusedOnTheirSize_data()
    {
        QTest::addColumn<quint32>("w");
        QTest::addColumn<quint32>("h");
        QTest::newRow("20000x20000") << quint32(20000) << quint32(20000);
        QTest::newRow("40000x40000") << quint32(40000) << quint32(40000);
        QTest::newRow("tall") << quint32(1) << quint32(50000);
    }
    void pixelBombsAreRefusedOnTheirSize()
    {
        QFETCH(quint32, w);
        QFETCH(quint32, h);
        QByteArray bomb = pngClaiming(w, h);
        QVERIFY(bomb.size() < 100);
        QBuffer buf(&bomb);
        buf.open(QIODevice::ReadOnly);
        QString err;
        QVERIFY(shot::decodeImage(&buf, &err).isNull());
        QVERIFY2(err.startsWith(u"The image is too large"_s), qPrintable(err));

        // The same from a file, which is how it arrives.
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        const QString path = dir.filePath(u"bomb.png"_s);
        QVERIFY(writeFile(path, bomb));
        err.clear();
        QVERIFY(shot::decodeFile(path, &err).isNull());
        QVERIFY2(err.startsWith(u"The image is too large"_s), qPrintable(err));
    }

    void notAnImageGetsTheGenericMessage()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        QByteArray noise(4096, 0);
        quint32 x = 12345;
        for (char &c : noise) {
            x = x * 1664525u + 1013904223u;
            c = char(x >> 24);
        }
        noise[0] = char(0x01);
        const QList<QPair<QString, QByteArray>> files = {
            {u"svg.png"_s, "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10'/></svg>"},
            {u"xmlsvg.png"_s, "<?xml version='1.0'?>\n<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/>"},
            {u"pdf.png"_s, "%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n"},
            {u"random.png"_s, noise},
            {u"empty.png"_s, QByteArray()},
        };
        for (const auto &[name, bytes] : files) {
            const QString path = dir.filePath(name);
            QVERIFY(writeFile(path, bytes));
            QString err;
            QVERIFY2(shot::decodeFile(path, &err).isNull(), qPrintable(name));
            if (bytes.isEmpty())
                QVERIFY2(!err.isEmpty(), qPrintable(name));
            else
                QVERIFY2(err == u"This is not an image the editor can open."_s, qPrintable(name + u": "_s + err));
        }
    }

    void decodeFileOpensOnlyRegularFiles()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        QString err;

        // A symlink to a regular PNG opens (people open linked files).
        const QString real = dir.filePath(u"real.png"_s);
        QVERIFY(randomImage(24, 16, QImage::Format_RGB32).save(real));
        const QString link = dir.filePath(u"link.png"_s);
        QVERIFY(QFile::link(real, link));
        QCOMPARE(shot::decodeFile(link, &err).size(), QSize(24, 16));
        QVERIFY(err.isEmpty());

        // A dangling link, a folder, a device.
        const QString dangling = dir.filePath(u"dangling.png"_s);
        QVERIFY(QFile::link(dir.filePath(u"nowhere.png"_s), dangling));
        QVERIFY(shot::decodeFile(dangling, &err).isNull());
        QVERIFY(!err.isEmpty());
        err.clear();
        QVERIFY(shot::decodeFile(dir.path(), &err).isNull());
        QCOMPARE(err, u"This is not a file."_s);
        err.clear();
        QVERIFY(shot::decodeFile(u"/dev/zero"_s, &err).isNull());
        QCOMPARE(err, u"This is not a file."_s);

        // Larger than the cap: a sparse file, so nothing is written, and it is refused without reading.
        const QString big = dir.filePath(u"big.png"_s);
        {
            QFile f(big);
            QVERIFY(f.open(QIODevice::WriteOnly));
            QVERIFY(f.write(pngBytes(randomImage(8, 8, QImage::Format_RGB32))) > 0);
            QVERIFY(f.resize(Backend::kMaxInputBytes + 1));
        }
        err.clear();
        QVERIFY(shot::decodeFile(big, &err).isNull());
        QCOMPARE(err, u"The file is too large."_s);
    }

    void decodeFileDoesNotBlockOnAFifo()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        const QString fifo = dir.filePath(u"pipe.png"_s);
        QVERIFY(::mkfifo(QFile::encodeName(fifo).constData(), 0600) == 0);

        // Through a link too: the check is on what is opened, not on the name.
        const QString link = dir.filePath(u"pipelink.png"_s);
        QVERIFY(QFile::link(fifo, link));

        for (const QString &path : {fifo, link}) {
            auto fut = std::async(std::launch::async, [path] {
                QString err;
                const bool null = shot::decodeFile(path, &err).isNull();
                return std::pair(null, err);
            });
            if (fut.wait_for(std::chrono::seconds(5)) != std::future_status::ready) {
                // Free the reader that waits for a writer, so the test can end.
                const int fd = ::open(QFile::encodeName(path).constData(), O_WRONLY);
                if (fd >= 0)
                    ::close(fd);
                fut.wait();
                QFAIL("decodeFile hung on a FIFO");
            }
            const auto [null, err] = fut.get();
            QVERIFY(null);
            QCOMPARE(err, u"This is not a file."_s);
        }
    }

    void decodeImageStopsAtTheCap()
    {
        // A device that claims to be huge is refused up front.
        struct Endless : QIODevice {
            qint64 size() const override { return Backend::kMaxInputBytes + 1; }
            qint64 readData(char *d, qint64 n) override { memset(d, 0, n); return n; }
            qint64 writeData(const char *, qint64) override { return -1; }
        } endless;
        endless.open(QIODevice::ReadOnly);
        QString err;
        QVERIFY(shot::decodeImage(&endless, &err).isNull());
        QCOMPARE(err, u"The file is too large."_s);
    }

    void saveAsWritesAtomically()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        Backend be;
        const QString src = dir.filePath(u"in.png"_s);
        const QImage in = randomImage(64, 48, QImage::Format_RGB32);
        QVERIFY(in.save(src));
        QSignalSpy loaded(&be, &Backend::loaded);
        be.load(src);
        QVERIFY(waitFor([&] { return loaded.count() == 1; }));
        QCOMPARE(be.imageSize(), QSize(64, 48));
        QCOMPARE(be.sourceName(), u"in.png"_s);

        const QString target = dir.filePath(u"out.png"_s);
        QVERIFY(be.resolveSavePath(target).value(u"exists"_s).toBool() == false);
        QCOMPARE(be.saveAs(target, {redact(0, 0, 10, 10, u"#000000"_s)}, QRectF(0, 0, 32, 32)), QString());
        QImage out(target);
        QCOMPARE(out.size(), QSize(32, 32));
        QCOMPARE(out.pixel(5, 5), 0xff000000u);
        QCOMPARE(out.pixel(20, 20), in.pixel(20, 20));
        // Replacing: no temporary file stays next to it.
        QVERIFY(be.resolveSavePath(target).value(u"exists"_s).toBool());
        QCOMPARE(be.saveAs(target, {}, QRectF()), QString());
        QCOMPARE(QImage(target).size(), QSize(64, 48));
        QStringList names = QDir(dir.path()).entryList(QDir::Files);
        names.sort();
        QCOMPARE(names, (QStringList{u"in.png"_s, u"out.png"_s}));

        // No suffix gives .png; jpg is fine; others and bad folders are refused.
        QCOMPARE(be.resolveSavePath(dir.filePath(u"plain"_s)).value(u"path"_s).toString(), dir.filePath(u"plain.png"_s));
        QCOMPARE(be.saveAs(dir.filePath(u"x.jpg"_s), {}, QRectF()), QString());
        QVERIFY(QFile::exists(dir.filePath(u"x.jpg"_s)));
        QVERIFY(!be.resolveSavePath(dir.filePath(u"x.txt"_s)).value(u"error"_s).toString().isEmpty());
        QVERIFY(!be.resolveSavePath(u"relative.png"_s).value(u"error"_s).toString().isEmpty());
        QVERIFY(!be.resolveSavePath(u""_s).value(u"error"_s).toString().isEmpty());
        QVERIFY(!be.resolveSavePath(dir.path()).value(u"error"_s).toString().isEmpty());
        QVERIFY(!be.resolveSavePath(dir.filePath(u"no/such/dir/x.png"_s)).value(u"error"_s).toString().isEmpty());
        QVERIFY(!be.saveAs(dir.filePath(u"no/such/dir/x.png"_s), {}, QRectF()).isEmpty());
    }

    void saveAsMakesNewFilesPrivate()
    {
        // Like the CLI's own files (0600), whatever the umask; a file that is
        // replaced keeps the mode it had.
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        const mode_t oldMask = ::umask(022);
        Backend be;
        const QString src = dir.filePath(u"in.png"_s);
        QVERIFY(randomImage(16, 16, QImage::Format_RGB32).save(src));
        QSignalSpy loaded(&be, &Backend::loaded);
        be.load(src);
        QVERIFY(waitFor([&] { return loaded.count() == 1; }));

        auto mode = [](const QString &path) {
            struct stat st;
            return ::stat(QFile::encodeName(path).constData(), &st) == 0 ? int(st.st_mode & 0777) : -1;
        };
        const QString fresh = dir.filePath(u"fresh.png"_s);
        QCOMPARE(be.saveAs(fresh, {}, QRectF()), QString());
        QCOMPARE(mode(fresh), 0600);

        const QString jpg = dir.filePath(u"fresh.jpg"_s);
        QCOMPARE(be.saveAs(jpg, {}, QRectF()), QString());
        QCOMPARE(mode(jpg), 0600);

        const QString old = dir.filePath(u"old.png"_s);
        QVERIFY(writeFile(old, "x"));
        QVERIFY(QFile::setPermissions(old, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ReadGroup));
        QCOMPARE(be.saveAs(old, {}, QRectF()), QString());
        QCOMPARE(mode(old), 0640);
        ::umask(oldMask);
    }

    void qmlShowsOutsideTextAsPlainText()
    {
        // File names and CLI error text reach the QML. No Text/Label there
        // may format them as rich text: only the Telamon.Ui labels, which are
        // plain, and a rich-text format is never asked for.
        QDir qml(QStringLiteral(QML_SOURCE_DIR));
        const QStringList files = qml.entryList({u"*.qml"_s}, QDir::Files);
        QVERIFY(!files.isEmpty());
        for (const QString &name : files) {
            QFile f(qml.filePath(name));
            QVERIFY(f.open(QIODevice::ReadOnly));
            const QString text = QString::fromUtf8(f.readAll());
            for (const QString &bad : {u"Text.RichText"_s, u"Text.AutoText"_s, u"Text.StyledText"_s,
                                       u"TextEdit.RichText"_s, u"TextEdit.AutoText"_s, u"TextArea"_s}) {
                QVERIFY2(!text.contains(bad), qPrintable(name + u" uses "_s + bad));
            }
            // A bare QtQuick Text or a Controls Label has an automatic format.
            const QRegularExpression bare(u"(^|[^A-Za-z0-9_.])(Text|Label|QQC2\\.Label|Controls\\.Label)\\s*\\{"_s);
            QVERIFY2(!bare.match(text).hasMatch(), qPrintable(name + u" has a bare Text or Label"_s));
        }
    }

    void helpersTalkToTheCli()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        qputenv("FAKE_CLI_DIR", dir.path().toUtf8());
        qunsetenv("FAKE_CLI_FAIL");

        Backend be;
        const QString src = dir.filePath(u"in.png"_s);
        const QImage in = randomImage(40, 30, QImage::Format_RGB32);
        QVERIFY(in.save(src));
        QSignalSpy loaded(&be, &Backend::loaded);
        be.load(src);
        QVERIFY(waitFor([&] { return loaded.count() == 1; }));

        QSignalSpy done(&be, &Backend::helperFinished);
        QVERIFY(be.save({redact(0, 0, 5, 5, u"#000000"_s)}, QRectF()));
        QVERIFY(be.helperBusy());
        QVERIFY(waitFor([&] { return done.count() == 1; }));
        QCOMPARE(done[0][0].toString(), u"save"_s);
        QVERIFY2(done[0][1].toBool(), qPrintable(done[0][2].toString()));
        const QString saved = done[0][2].toString();
        QCOMPARE(saved, dir.filePath(u"Screenshot_1.png"_s));
        QCOMPARE(QImage(saved).pixel(2, 2), 0xff000000u);
        QCOMPARE(QImage(saved).pixel(20, 20), in.pixel(20, 20));
        QVERIFY(!be.helperBusy());

        done.clear();
        QVERIFY(be.copy({}, QRectF(5, 5, 10, 10)));
        QVERIFY(waitFor([&] { return done.count() == 1; }));
        QVERIFY(done[0][1].toBool());
        QCOMPARE(done[0][0].toString(), u"copy"_s);
        QCOMPARE(QImage(dir.filePath(u"clipboard.png"_s)).size(), QSize(10, 10));

        const QString log = [&] { QFile f(dir.filePath(u"args.log"_s)); (void)f.open(QIODevice::ReadOnly); return QString::fromUtf8(f.readAll()); }();
        QCOMPARE(log, u"--save-png\n--copy-png\n"_s);

        // A failing CLI: the reason arrives, plain, and the editor goes on.
        qputenv("FAKE_CLI_FAIL", "Screenshots folder is not writable\x1b[31m");
        done.clear();
        QVERIFY(be.save({}, QRectF()));
        QVERIFY(waitFor([&] { return done.count() == 1; }));
        QVERIFY(!done[0][1].toBool());
        QCOMPARE(done[0][2].toString(), u"Screenshots folder is not writable [31m"_s);
        QVERIFY(!be.helperBusy());
        qunsetenv("FAKE_CLI_FAIL");

        // A CLI that is not there.
        Backend::setCliPathForTests(u"/nonexistent/telamon-screenshot"_s);
        done.clear();
        be.save({}, QRectF());
        QVERIFY(waitFor([&] { return done.count() == 1; }));
        QVERIFY(!done[0][1].toBool());
        QVERIFY(!done[0][2].toString().isEmpty());
        QVERIFY(!be.helperBusy());
    }

    void startCaptureRunsTheCliDetached()
    {
        QTemporaryDir dir;
        QVERIFY(dir.isValid());
        qputenv("FAKE_CLI_DIR", dir.path().toUtf8());
        Backend be;
        QVERIFY(!be.startCapture(u"; rm -rf /"_s, 0));
        QVERIFY(!be.startCapture(u"--save-png"_s, 0));
        QVERIFY(be.startCapture(u"region"_s, 0));
        QVERIFY(be.startCapture(u"active-window"_s, 7));
        QVERIFY(be.startCapture(u"screen"_s, 999));
        const QString logPath = dir.filePath(u"args.log"_s);
        auto lines = [&] { QFile f(logPath); if (!f.open(QIODevice::ReadOnly)) return QStringList(); return QString::fromUtf8(f.readAll()).split(u'\n', Qt::SkipEmptyParts); };
        QVERIFY(waitFor([&] { return lines().size() == 3; }));
        QStringList got = lines();
        got.sort();
        QCOMPARE(got, (QStringList{u"--active-window --delay 7 --edit"_s, u"--region --edit"_s, u"--screen --delay 60 --edit"_s}));
    }
};

QTEST_MAIN(TstBackend)
#include "tst_backend.moc"
