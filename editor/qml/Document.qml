import QtQuick

// The annotations on one image, and their history. It knows nothing of the
// screen: positions are in IMAGE pixels, so the export at the native size is
// exact. Items are plain objects and are never changed once added, so a
// history entry is only the list and the crop as they were.
//
// Item shapes (all with `type`, `color` as "#rrggbb", `size`):
//   arrow        { x1, y1, x2, y2 }
//   rect         { x, y, w, h }
//   pen          { pts: [x0, y0, x1, y1, ...] }
//   highlighter  { pts: [...] }
//   text         { x, y, text }
//   marker       { x, y, number }
//   redact       { x, y, w, h }          (always an opaque fill)
QtObject {
    id: doc

    // The committed items, oldest first. A new array each time it changes.
    property var items: []
    // The crop as { x, y, w, h } in image pixels, or null for the whole image.
    property var crop: null

    readonly property bool canUndo: _past.length > 0
    readonly property bool canRedo: _future.length > 0
    // Annotated or cropped (what "Discard Changes?" is about when not saved).
    readonly property bool hasAnnotations: items.length > 0 || crop !== null
    // Different from the state at the last save (or at the start).
    readonly property bool dirty: _stateId !== _savedId
    // Annotated or cropped, and not saved: closing asks first.
    readonly property bool needsSave: dirty && hasAnnotations

    property var _past: []
    property var _future: []
    property int _stateId: 0
    property int _savedId: 0
    property int _counter: 0

    signal changed()

    function _snapshot() {
        return { items: doc.items, crop: doc.crop, id: doc._stateId };
    }

    function _commit(newItems, newCrop) {
        doc._past = doc._past.concat([doc._snapshot()]);
        doc._future = [];
        doc.items = newItems;
        doc.crop = newCrop;
        doc._stateId = ++doc._counter;
        doc.changed();
    }

    function addItem(item) {
        if (!item || typeof item.type !== "string")
            return false;
        doc._commit(doc.items.concat([item]), doc.crop);
        return true;
    }

    // Sets the crop; null (or a rectangle that is the whole image) removes it.
    // One entry in the history either way.
    function setCrop(rect) {
        if (doc._sameRect(rect, doc.crop))
            return false;
        doc._commit(doc.items, rect ? { x: rect.x, y: rect.y, w: rect.w, h: rect.h } : null);
        return true;
    }

    function _sameRect(a, b) {
        if (!a || !b)
            return !a && !b;
        return a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
    }

    function undo() {
        if (!doc.canUndo)
            return false;
        const past = doc._past;
        const prev = past[past.length - 1];
        doc._future = doc._future.concat([doc._snapshot()]);
        doc._past = past.slice(0, past.length - 1);
        doc._restore(prev);
        return true;
    }

    function redo() {
        if (!doc.canRedo)
            return false;
        const fut = doc._future;
        const next = fut[fut.length - 1];
        doc._past = doc._past.concat([doc._snapshot()]);
        doc._future = fut.slice(0, fut.length - 1);
        doc._restore(next);
        return true;
    }

    function _restore(snap) {
        doc.items = snap.items;
        doc.crop = snap.crop;
        doc._stateId = snap.id;
        doc.changed();
    }

    // The number the next marker gets: one more than the highest on the
    // image, so an undone marker's number is handed out again.
    function nextMarkerNumber() {
        let max = 0;
        for (const it of doc.items) {
            if (it.type === "marker" && it.number > max)
                max = it.number;
        }
        return max + 1;
    }

    // The state with this id (default: the current one) is on disk.
    function stateId() {
        return doc._stateId;
    }

    function markSaved(id) {
        doc._savedId = id === undefined ? doc._stateId : id;
    }

    // A new image: no items, no history.
    function reset() {
        doc.items = [];
        doc.crop = null;
        doc._past = [];
        doc._future = [];
        doc._stateId = 0;
        doc._savedId = 0;
        doc._counter = 0;
        doc.changed();
    }
}
