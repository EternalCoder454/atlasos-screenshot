// Export, decoding limits, atomic save and the CLI helpers (against the fake CLI).
#include "backend.h"
#include "render.h"

#include <QBuffer>
#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QImage>
#include <QImageWriter>
#include <QRandomGenerator>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

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
    void initTestCase() { m_cli = qgetenv("TELAMON_SCREENSHOT_BIN"); }
    void init() { qputenv("TELAMON_SCREENSHOT_BIN", m_cli); }

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
        qputenv("TELAMON_SCREENSHOT_BIN", "/nonexistent/telamon-screenshot");
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
