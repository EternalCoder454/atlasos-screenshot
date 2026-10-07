#include "canvasview.h"

#include <QPainter>
#include <QPainterPath>
#include <QQuickWindow>
#include <QtMath>

CanvasView::CanvasView(QQuickItem *parent)
    : QQuickPaintedItem(parent)
{
    setAntialiasing(true);
    setOpaquePainting(false);
}

QRectF CanvasView::effectiveViewRect() const
{
    const QRectF all(QPointF(0, 0), QSizeF(m_image.size()));
    if (m_viewRect.isNull())
        return all;
    const QRectF r = m_viewRect.intersected(all);
    return r.isEmpty() ? all : r;
}

void CanvasView::updateLayout()
{
    const QRectF v = effectiveViewRect();
    qreal s = 1;
    QPointF off;
    if (!v.isEmpty() && width() > 0 && height() > 0) {
        s = qMin(qMin(width() / v.width(), height() / v.height()), m_maxScale);
        off = QPointF((width() - v.width() * s) / 2, (height() - v.height() * s) / 2);
    }
    if (!qFuzzyCompare(s, m_scale) || off != m_offset) {
        m_scale = s;
        m_offset = off;
        emit layoutChanged();
    }
    update();
}

void CanvasView::setImage(const QImage &image)
{
    if (m_image.cacheKey() == image.cacheKey() && m_image.size() == image.size())
        return;
    m_image = image;
    m_scaled = QImage();
    updateLayout();
    emit imageChanged();
}

void CanvasView::setItems(const QVariantList &items)
{
    m_itemsVariant = items;
    m_items = shot::parseAnnotations(items);
    update();
    emit itemsChanged();
}

void CanvasView::setDraft(const QVariantMap &draft)
{
    m_draftVariant = draft;
    m_hasDraft = !draft.isEmpty() && shot::parseAnnotation(draft, &m_draft);
    update();
    emit draftChanged();
}

void CanvasView::setViewRect(const QRectF &r)
{
    if (m_viewRect == r)
        return;
    m_viewRect = r;
    updateLayout();
    emit viewRectChanged();
}

void CanvasView::setClearRect(const QRectF &r)
{
    if (m_clearRect == r)
        return;
    m_clearRect = r;
    update();
    emit clearRectChanged();
}

void CanvasView::setMaxScale(qreal s)
{
    if (!(s > 0) || qFuzzyCompare(s, m_maxScale))
        return;
    m_maxScale = s;
    updateLayout();
    emit maxScaleChanged();
}

void CanvasView::geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry)
{
    QQuickPaintedItem::geometryChange(newGeometry, oldGeometry);
    updateLayout();
}

QPointF CanvasView::toImage(qreal x, qreal y) const
{
    const QRectF v = effectiveViewRect();
    return QPointF((x - m_offset.x()) / m_scale + v.x(), (y - m_offset.y()) / m_scale + v.y());
}

QPointF CanvasView::toItem(qreal x, qreal y) const
{
    const QRectF v = effectiveViewRect();
    return QPointF((x - v.x()) * m_scale + m_offset.x(), (y - v.y()) * m_scale + m_offset.y());
}

void CanvasView::paint(QPainter *painter)
{
    if (m_image.isNull())
        return;
    const QRectF v = effectiveViewRect();
    const qreal dpr = window() ? window()->effectiveDevicePixelRatio() : 1.0;
    painter->setRenderHint(QPainter::Antialiasing, true);

    const QRectF target(m_offset, QSizeF(v.width() * m_scale, v.height() * m_scale));
    if (m_scale * dpr < 1.0) {
        // Shrinking: scale once, with the smooth filter, and draw that until
        // the size or the image changes.
        const QSize px(qMax(1, qRound(target.width() * dpr)), qMax(1, qRound(target.height() * dpr)));
        if (m_scaled.isNull() || m_scaled.size() != px || m_scaledRect != v || m_scaledKey != m_image.cacheKey()) {
            const QRect src = v.toAlignedRect().intersected(QRect(QPoint(0, 0), m_image.size()));
            m_scaled = m_image.copy(src).scaled(px, Qt::IgnoreAspectRatio, Qt::SmoothTransformation);
            m_scaled.setDevicePixelRatio(1.0);
            m_scaledRect = v;
            m_scaledKey = m_image.cacheKey();
        }
        painter->drawImage(target, m_scaled);
    } else {
        // At 1:1 or bigger: the pixels as they are, not smoothed.
        painter->setRenderHint(QPainter::SmoothPixmapTransform, false);
        painter->drawImage(target, m_image, v);
        painter->setRenderHint(QPainter::SmoothPixmapTransform, true);
    }

    painter->save();
    painter->setClipRect(target);
    painter->translate(m_offset);
    painter->scale(m_scale, m_scale);
    painter->translate(-v.topLeft());
    for (const shot::Annotation &a : std::as_const(m_items))
        shot::paintAnnotation(*painter, a);
    if (m_hasDraft)
        shot::paintAnnotation(*painter, m_draft);
    painter->restore();

    if (!m_clearRect.isNull()) {
        // The part outside the crop, dimmed in one fill, so there is no seam.
        const QRectF clear(toItem(m_clearRect.left(), m_clearRect.top()), toItem(m_clearRect.right(), m_clearRect.bottom()));
        QPainterPath dim;
        dim.setFillRule(Qt::OddEvenFill);
        dim.addRect(target);
        dim.addRect(clear.intersected(target));
        painter->setRenderHint(QPainter::Antialiasing, false);
        painter->fillPath(dim, QColor(0, 0, 0, 140));
    }
}
