#include "render.h"

#include <QBuffer>
#include <QFont>
#include <QGuiApplication>
#include <QImageWriter>
#include <QPainter>
#include <QPainterPath>
#include <QtMath>

#include <cmath>

namespace shot {

namespace {

bool finite(qreal v)
{
    return std::isfinite(v) && std::abs(v) <= kMaxCoord;
}

bool num(const QVariantMap &m, const char *key, qreal *out)
{
    const QVariant v = m.value(QLatin1String(key));
    bool ok = false;
    const qreal d = v.toDouble(&ok);
    if (!ok || !finite(d))
        return false;
    *out = d;
    return true;
}

QFont textFont(qreal px, bool bold)
{
    QFont f = QGuiApplication::font();
    f.setPixelSize(qMax(1, qRound(px)));
    f.setWeight(bold ? QFont::DemiBold : QFont::Medium);
    return f;
}

QPainterPath smoothPath(const QList<QPointF> &pts)
{
    QPainterPath path;
    if (pts.isEmpty())
        return path;
    path.moveTo(pts.first());
    if (pts.size() == 1) {
        path.lineTo(pts.first() + QPointF(0.01, 0));
        return path;
    }
    if (pts.size() == 2) {
        path.lineTo(pts.last());
        return path;
    }
    // Quadratic curves through the midpoints, with the points as controls:
    // smooths the hand tremor lightly and keeps the ends where they are.
    for (int i = 1; i < pts.size() - 1; ++i) {
        const QPointF mid = (pts[i] + pts[i + 1]) / 2;
        path.quadTo(pts[i], mid);
    }
    path.lineTo(pts.last());
    return path;
}

void paintArrow(QPainter &p, const Annotation &a)
{
    const QPointF from = a.p1, to = a.p2;
    const qreal len = QLineF(from, to).length();
    if (len < 0.5)
        return;
    const qreal w = a.size;
    const qreal headLen = qMin(len * 0.8, w * 4.0 + 8.0);
    const qreal headHalf = headLen * 0.5;
    const QPointF dir = (to - from) / len;
    const QPointF normal(-dir.y(), dir.x());
    const QPointF baseCentre = to - dir * headLen;

    p.save();
    p.setRenderHint(QPainter::Antialiasing, true);
    QPen pen(a.color, w, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin);
    p.setPen(pen);
    // The shaft stops inside the head, so the tip stays sharp.
    p.drawLine(from, baseCentre + dir * (headLen * 0.3));
    p.setPen(Qt::NoPen);
    p.setBrush(a.color);
    const QPointF tri[3] = {to, baseCentre + normal * headHalf, baseCentre - normal * headHalf};
    p.drawPolygon(tri, 3);
    p.restore();
}

void paintMarker(QPainter &p, const Annotation &a)
{
    const qreal d = a.size;
    const QRectF circle(a.p1.x() - d / 2, a.p1.y() - d / 2, d, d);
    p.save();
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(QPen(QColor(255, 255, 255, 230), qMax(1.0, d * 0.07)));
    p.setBrush(a.color);
    p.drawEllipse(circle);
    // Light digits on a dark disc, dark on a light one.
    const qreal lum = 0.299 * a.color.redF() + 0.587 * a.color.greenF() + 0.114 * a.color.blueF();
    p.setPen(lum > 0.62 ? QColor(0, 0, 0) : QColor(255, 255, 255));
    const qreal fontPx = a.text.size() > 2 ? d * 0.4 : (a.text.size() > 1 ? d * 0.5 : d * 0.6);
    p.setFont(textFont(fontPx, true));
    p.drawText(circle, Qt::AlignCenter | Qt::TextSingleLine, a.text);
    p.restore();
}

} // namespace

bool parseAnnotation(const QVariantMap &m, Annotation *out)
{
    Annotation a;
    const QString type = m.value(QStringLiteral("type")).toString();
    if (type == QLatin1String("arrow"))
        a.type = Annotation::Arrow;
    else if (type == QLatin1String("rect"))
        a.type = Annotation::Rect;
    else if (type == QLatin1String("pen"))
        a.type = Annotation::Pen;
    else if (type == QLatin1String("highlighter"))
        a.type = Annotation::Highlighter;
    else if (type == QLatin1String("text"))
        a.type = Annotation::Text;
    else if (type == QLatin1String("marker"))
        a.type = Annotation::Marker;
    else if (type == QLatin1String("redact"))
        a.type = Annotation::Redact;
    else
        return false;

    a.color = QColor(m.value(QStringLiteral("color")).toString());
    if (!a.color.isValid())
        a.color = Qt::black;
    // The fill of a redaction is always opaque; the others take the colour's
    // own alpha (the highlighter adds its own).
    if (a.type == Annotation::Redact)
        a.color.setAlpha(255);

    qreal size = 4;
    if (num(m, "size", &size))
        a.size = qBound<qreal>(1, size, 4000);

    switch (a.type) {
    case Annotation::Arrow: {
        qreal x1, y1, x2, y2;
        if (!num(m, "x1", &x1) || !num(m, "y1", &y1) || !num(m, "x2", &x2) || !num(m, "y2", &y2))
            return false;
        a.p1 = {x1, y1};
        a.p2 = {x2, y2};
        break;
    }
    case Annotation::Rect:
    case Annotation::Redact: {
        qreal x, y, w, h;
        if (!num(m, "x", &x) || !num(m, "y", &y) || !num(m, "w", &w) || !num(m, "h", &h))
            return false;
        const QRectF r = QRectF(x, y, w, h).normalized();
        a.p1 = r.topLeft();
        a.p2 = r.bottomRight();
        break;
    }
    case Annotation::Pen:
    case Annotation::Highlighter: {
        const QVariantList flat = m.value(QStringLiteral("pts")).toList();
        if (flat.size() < 2 || flat.size() / 2 > kMaxPoints)
            return false;
        a.pts.reserve(flat.size() / 2);
        for (int i = 0; i + 1 < flat.size(); i += 2) {
            bool okx = false, oky = false;
            const qreal x = flat[i].toDouble(&okx), y = flat[i + 1].toDouble(&oky);
            if (!okx || !oky || !finite(x) || !finite(y))
                return false;
            a.pts.append({x, y});
        }
        break;
    }
    case Annotation::Text:
    case Annotation::Marker: {
        qreal x, y;
        if (!num(m, "x", &x) || !num(m, "y", &y))
            return false;
        a.p1 = {x, y};
        if (a.type == Annotation::Text) {
            a.text = m.value(QStringLiteral("text")).toString().left(kMaxTextLength);
            if (a.text.isEmpty())
                return false;
        } else {
            a.text = QString::number(qBound(0, m.value(QStringLiteral("number")).toInt(), 999999));
        }
        break;
    }
    }
    *out = a;
    return true;
}

QList<Annotation> parseAnnotations(const QVariantList &list)
{
    QList<Annotation> out;
    out.reserve(qMin<int>(list.size(), kMaxItems));
    for (const QVariant &v : list) {
        if (out.size() >= kMaxItems)
            break;
        Annotation a;
        if (parseAnnotation(v.toMap(), &a))
            out.append(a);
    }
    return out;
}

QRect redactRect(const Annotation &a)
{
    const int x0 = qFloor(a.p1.x()), y0 = qFloor(a.p1.y());
    const int x1 = qCeil(a.p2.x()), y1 = qCeil(a.p2.y());
    return QRect(QPoint(x0, y0), QPoint(x1 - 1, y1 - 1));
}

void paintAnnotation(QPainter &p, const Annotation &a)
{
    switch (a.type) {
    case Annotation::Arrow:
        paintArrow(p, a);
        break;
    case Annotation::Rect: {
        p.save();
        p.setRenderHint(QPainter::Antialiasing, true);
        p.setPen(QPen(a.color, a.size, Qt::SolidLine, Qt::SquareCap, Qt::MiterJoin));
        p.setBrush(Qt::NoBrush);
        p.drawRect(QRectF(a.p1, a.p2));
        p.restore();
        break;
    }
    case Annotation::Pen: {
        p.save();
        p.setRenderHint(QPainter::Antialiasing, true);
        p.setPen(QPen(a.color, a.size, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
        p.setBrush(Qt::NoBrush);
        p.drawPath(smoothPath(a.pts));
        p.restore();
        break;
    }
    case Annotation::Highlighter: {
        p.save();
        p.setRenderHint(QPainter::Antialiasing, true);
        // One stroked path: overlaps inside a stroke do not darken.
        QColor c = a.color;
        c.setAlpha(110);
        p.setPen(QPen(c, a.size * 4.0, Qt::SolidLine, Qt::SquareCap, Qt::RoundJoin));
        p.setBrush(Qt::NoBrush);
        p.drawPath(smoothPath(a.pts));
        p.restore();
        break;
    }
    case Annotation::Text: {
        p.save();
        p.setRenderHint(QPainter::Antialiasing, true);
        p.setRenderHint(QPainter::TextAntialiasing, true);
        p.setPen(a.color);
        p.setFont(textFont(a.size, false));
        // drawText takes plain text: no markup, whatever the user typed.
        p.drawText(QRectF(a.p1, QSizeF(kMaxCoord, kMaxCoord)), Qt::AlignLeft | Qt::AlignTop | Qt::TextDontClip, a.text);
        p.restore();
        break;
    }
    case Annotation::Marker:
        paintMarker(p, a);
        break;
    case Annotation::Redact: {
        p.save();
        p.setRenderHint(QPainter::Antialiasing, false);
        // Source, not SourceOver: the pixels become exactly this colour at
        // full alpha, whatever was below.
        p.setCompositionMode(QPainter::CompositionMode_Source);
        p.fillRect(redactRect(a), a.color);
        p.restore();
        break;
    }
    }
}

QImage renderExport(const QImage &base, const QList<Annotation> &items, const QRectF &crop)
{
    if (base.isNull())
        return {};
    const QRect all(QPoint(0, 0), base.size());
    QRect box = all;
    if (!crop.isNull()) {
        // Whole pixels, inside the image.
        const QRect c(QPoint(qFloor(crop.left()), qFloor(crop.top())), QPoint(qCeil(crop.right()) - 1, qCeil(crop.bottom()) - 1));
        box = c.intersected(all);
    }
    if (box.isEmpty())
        return {};
    if (items.isEmpty())
        return base.copy(box);

    // The base's own kind of pixels, so those the annotations do not touch
    // stay as they were (premultiplying would round a half-transparent one).
    const bool alpha = base.hasAlphaChannel();
    QImage out(box.size(), alpha ? QImage::Format_ARGB32 : QImage::Format_RGB32);
    if (out.isNull())
        return {};
    QPainter p(&out);
    p.setCompositionMode(QPainter::CompositionMode_Source);
    p.drawImage(QPoint(0, 0), base, box);
    p.setCompositionMode(QPainter::CompositionMode_SourceOver);
    p.translate(-box.topLeft());
    p.setClipRect(QRectF(box));
    for (const Annotation &a : items)
        paintAnnotation(p, a);
    p.end();
    return out;
}

QByteArray encodePng(const QImage &image)
{
    QByteArray bytes;
    QBuffer buf(&bytes);
    buf.open(QIODevice::WriteOnly);
    QImageWriter w(&buf, "png");
    if (!w.write(image))
        return {};
    return bytes;
}

} // namespace shot
