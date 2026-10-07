// Drawing of the annotations, shared by the canvas on screen and the export,
// so what is seen is what is saved. Everything is in IMAGE pixels.
#pragma once

#include <QByteArray>
#include <QColor>
#include <QImage>
#include <QList>
#include <QPointF>
#include <QRect>
#include <QRectF>
#include <QString>
#include <QVariantList>
#include <QVariantMap>

class QPainter;

namespace shot {

struct Annotation {
    enum Type { Arrow, Rect, Pen, Highlighter, Text, Marker, Redact };
    Type type = Rect;
    QColor color = Qt::black;
    // Stroke width (arrow, rectangle, pen; the highlighter is wider), font
    // size in pixels (text) or diameter (marker).
    qreal size = 4;
    QPointF p1, p2;       // arrow ends; rectangle and redaction corners; text and marker position in p1
    QList<QPointF> pts;   // pen and highlighter
    QString text;         // text; the number for a marker
};

// Limits for what the document may hold, so a bad item cannot allocate or
// paint without bound.
constexpr int kMaxItems = 20000;
constexpr int kMaxPoints = 200000;
constexpr int kMaxTextLength = 4096;
constexpr qreal kMaxCoord = 1.0e6;

// One item of the document (a JS object): false when it is not a valid one.
bool parseAnnotation(const QVariantMap &map, Annotation *out);
QList<Annotation> parseAnnotations(const QVariantList &list);

void paintAnnotation(QPainter &p, const Annotation &a);

// The whole-pixel box a redaction fills.
QRect redactRect(const Annotation &a);

// The image with the annotations on it, cropped to `crop` (the whole image
// when it is null). A null image when the crop leaves nothing.
// With nothing to draw the result is the base copied, bit for bit.
QImage renderExport(const QImage &base, const QList<Annotation> &items, const QRectF &crop);

QByteArray encodePng(const QImage &image);

} // namespace shot
