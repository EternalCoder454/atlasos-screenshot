import QtQuick
import QtTest
import net.eterneon.telamon.screenshoteditor

TestCase {
    id: tc
    name: "Document"

    Component {
        id: docComp
        Document {}
    }

    function arrow() {
        return { type: "arrow", x1: 1, y1: 2, x2: 30, y2: 40, color: "#e5484d", size: 4 };
    }
    function marker(n) {
        return { type: "marker", x: 5, y: 5, number: n, color: "#e5484d", size: 30 };
    }

    function test_starts_empty_and_clean() {
        const d = createTemporaryObject(docComp, tc);
        compare(d.items.length, 0);
        compare(d.crop, null);
        verify(!d.canUndo);
        verify(!d.canRedo);
        verify(!d.dirty);
        verify(!d.needsSave);
        verify(!d.hasAnnotations);
    }

    function test_add_undo_redo() {
        const d = createTemporaryObject(docComp, tc);
        verify(d.addItem(arrow()));
        compare(d.items.length, 1);
        verify(d.canUndo);
        verify(d.dirty);
        verify(d.needsSave);
        verify(d.undo());
        compare(d.items.length, 0);
        verify(!d.dirty);
        verify(d.canRedo);
        verify(d.redo());
        compare(d.items.length, 1);
        verify(d.dirty);
        verify(!d.redo());
    }

    function test_new_item_clears_redo() {
        const d = createTemporaryObject(docComp, tc);
        d.addItem(arrow());
        d.undo();
        verify(d.canRedo);
        d.addItem(arrow());
        verify(!d.canRedo);
    }

    function test_rejects_bad_items() {
        const d = createTemporaryObject(docComp, tc);
        verify(!d.addItem(null));
        verify(!d.addItem({ x: 1 }));
        verify(!d.addItem({ type: 5 }));
        compare(d.items.length, 0);
        verify(!d.dirty);
    }

    function test_undo_on_empty_is_noop() {
        const d = createTemporaryObject(docComp, tc);
        verify(!d.undo());
        verify(!d.redo());
    }

    function test_items_are_a_new_array_each_time() {
        const d = createTemporaryObject(docComp, tc);
        const spy = createTemporaryObject(spyComp, tc, { target: d, signalName: "itemsChanged" });
        d.addItem(arrow());
        compare(spy.count, 1);
        d.undo();
        compare(spy.count, 2);
    }

    Component {
        id: spyComp
        SignalSpy {}
    }

    function test_marker_numbers() {
        const d = createTemporaryObject(docComp, tc);
        compare(d.nextMarkerNumber(), 1);
        d.addItem(marker(d.nextMarkerNumber()));
        d.addItem(arrow());
        d.addItem(marker(d.nextMarkerNumber()));
        d.addItem(marker(d.nextMarkerNumber()));
        compare(d.nextMarkerNumber(), 4);
        // Undo the last marker: its number is handed out again.
        d.undo();
        compare(d.nextMarkerNumber(), 3);
        d.undo();
        compare(d.nextMarkerNumber(), 2);
        d.redo();
        compare(d.nextMarkerNumber(), 3);
    }

    function test_crop_is_part_of_history() {
        const d = createTemporaryObject(docComp, tc);
        verify(d.setCrop({ x: 10, y: 20, w: 100, h: 50 }));
        compare(d.crop.w, 100);
        verify(d.dirty);
        verify(d.hasAnnotations);
        // The same crop again is not a new step.
        verify(!d.setCrop({ x: 10, y: 20, w: 100, h: 50 }));
        d.setCrop({ x: 0, y: 0, w: 30, h: 30 });
        compare(d.crop.w, 30);
        d.undo();
        compare(d.crop.w, 100);
        d.undo();
        compare(d.crop, null);
        verify(!d.dirty);
        d.redo();
        d.redo();
        compare(d.crop.w, 30);
        // Removing the crop is a step too.
        verify(d.setCrop(null));
        compare(d.crop, null);
        d.undo();
        compare(d.crop.w, 30);
    }

    function test_crop_keeps_items() {
        const d = createTemporaryObject(docComp, tc);
        d.addItem(arrow());
        d.setCrop({ x: 1, y: 1, w: 5, h: 5 });
        d.undo();
        compare(d.items.length, 1);
        compare(d.crop, null);
        d.undo();
        compare(d.items.length, 0);
    }

    function test_saved_state_and_dirty() {
        const d = createTemporaryObject(docComp, tc);
        d.addItem(arrow());
        d.markSaved();
        verify(!d.dirty);
        verify(!d.needsSave);
        d.addItem(arrow());
        verify(d.dirty);
        verify(d.needsSave);
        d.undo();
        verify(!d.dirty);       // back to the saved state
        d.undo();
        verify(d.dirty);        // before the save
        verify(!d.needsSave);   // ...but nothing is drawn: nothing to lose
        d.redo();
        verify(!d.dirty);
    }

    function test_save_of_an_older_state_leaves_later_changes_unsaved() {
        const d = createTemporaryObject(docComp, tc);
        d.addItem(arrow());
        const id = d.stateId();     // the save starts here...
        d.addItem(arrow());         // ...and the user draws on while it runs
        d.markSaved(id);
        verify(d.dirty);
        d.undo();
        verify(!d.dirty);
    }

    function test_reset() {
        const d = createTemporaryObject(docComp, tc);
        d.addItem(arrow());
        d.setCrop({ x: 1, y: 1, w: 5, h: 5 });
        d.reset();
        compare(d.items.length, 0);
        compare(d.crop, null);
        verify(!d.canUndo);
        verify(!d.canRedo);
        verify(!d.dirty);
        compare(d.nextMarkerNumber(), 1);
    }
}
