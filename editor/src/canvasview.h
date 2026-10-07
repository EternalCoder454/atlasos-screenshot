// The picture on screen: the image, the annotations and the one being drawn,
// fitted into the item. A QQuickPaintedItem, so it paints with the same
// code as the export (render.cpp).
#pragma once

#include "render.h"

#include <QImage>
#include <QPointF>
#include <QQuickPaintedItem>
#include <QRectF>
#include <QVariantList>
#include <QVariantMap>
#include <qqmlintegration.h>

class CanvasView : public QQuickPaintedItem
{
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QImage image READ image WRITE setImage NOTIFY imageChanged FINAL)
    Q_PROPERTY(QVariantList items READ items WRITE setItems NOTIFY itemsChanged FINAL)
    Q_PROPERTY(QVariantMap draft READ draft WRITE setDraft NOTIFY draftChanged FINAL)
    // The part of the image shown (the crop, else the whole image), in image pixels.
    Q_PROPERTY(QRectF viewRect READ viewRect WRITE setViewRect NOTIFY viewRectChanged FINAL)
    // A part of the image left undimmed while the rest is dimmed (the crop being edited), in image
    // pixels; null for no dimming.
    Q_PROPERTY(QRectF clearRect READ clearRect WRITE setClearRect NOTIFY clearRectChanged FINAL)
    // The largest scale the picture is shown at (1 image pixel is 1 device pixel at the most).
    Q_PROPERTY(qreal maxScale READ maxScale WRITE setMaxScale NOTIFY maxScaleChanged FINAL)
    Q_PROPERTY(qreal scale READ fitScale NOTIFY layoutChanged FINAL)
    Q_PROPERTY(QPointF offset READ offset NOTIFY layoutChanged FINAL)

public:
    explicit CanvasView(QQuickItem *parent = nullptr);

    QImage image() const { return m_image; }
    void setImage(const QImage &image);
    QVariantList items() const { return m_itemsVariant; }
    void setItems(const QVariantList &items);
    QVariantMap draft() const { return m_draftVariant; }
    void setDraft(const QVariantMap &draft);
    QRectF viewRect() const { return m_viewRect; }
    void setViewRect(const QRectF &r);
    QRectF clearRect() const { return m_clearRect; }
    void setClearRect(const QRectF &r);
    qreal maxScale() const { return m_maxScale; }
    void setMaxScale(qreal s);
    qreal fitScale() const { return m_scale; }
    QPointF offset() const { return m_offset; }

    // Item coordinates to image pixels, and back.
    Q_INVOKABLE QPointF toImage(qreal x, qreal y) const;
    Q_INVOKABLE QPointF toItem(qreal x, qreal y) const;

    void paint(QPainter *painter) override;

signals:
    void imageChanged();
    void itemsChanged();
    void draftChanged();
    void viewRectChanged();
    void clearRectChanged();
    void maxScaleChanged();
    void layoutChanged();

protected:
    void geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry) override;

private:
    void updateLayout();
    QRectF effectiveViewRect() const;

    QImage m_image;
    QVariantList m_itemsVariant;
    QList<shot::Annotation> m_items;
    QVariantMap m_draftVariant;
    bool m_hasDraft = false;
    shot::Annotation m_draft;
    QRectF m_viewRect;
    QRectF m_clearRect;
    qreal m_maxScale = 1.0;
    qreal m_scale = 1.0;
    QPointF m_offset;

    // The image scaled for the screen, kept while only the annotations change.
    QImage m_scaled;
    QRectF m_scaledRect;
    qreal m_scaledScale = 0;
    qint64 m_scaledKey = 0;
};
