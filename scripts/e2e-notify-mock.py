#!/usr/bin/python3
"""A stand-in for Plasma's notification server and for the file manager, for
scripts/e2e-kwin.sh (inside the image's private session bus only).

org.freedesktop.Notifications: every Notify is logged as one JSON line in
$MOCK_LOG. When $MOCK_ACTION_FILE holds an action key, that action of the latest
notification is "clicked" (ActionInvoked, then NotificationClosed) and the file
is emptied, so a test picks one button after it has seen the notification.
org.freedesktop.FileManager1: ShowItems is logged the same way.
"""
import json
import os

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

LOG = os.environ.get("MOCK_LOG", "/out/notify.log")
ACTION_FILE = os.environ.get("MOCK_ACTION_FILE", "/tmp/mock-action")


def log(entry):
    with open(LOG, "a") as f:
        f.write(json.dumps(entry) + "\n")


def plain(v):
    if isinstance(v, (dbus.String, str)):
        return str(v)
    if isinstance(v, dbus.Struct):
        return [plain(x) for x in v]
    if isinstance(v, (dbus.Array, list)):
        return [plain(x) for x in v]
    if isinstance(v, dbus.Dictionary):
        return {str(k): plain(x) for k, x in v.items()}
    if isinstance(v, dbus.Boolean):
        return bool(v)
    if isinstance(v, (dbus.Int32, dbus.UInt32, dbus.Byte, int)):
        return int(v)
    return str(v)


class Notifications(dbus.service.Object):
    def __init__(self, bus):
        super().__init__(bus, "/org/freedesktop/Notifications")
        self.next_id = 1
        self.last = None
        GLib.timeout_add(100, self.poll)

    @dbus.service.method("org.freedesktop.Notifications", in_signature="", out_signature="as")
    def GetCapabilities(self):
        return ["actions", "body", "body-markup", "icon-static"]

    @dbus.service.method("org.freedesktop.Notifications", in_signature="", out_signature="ssss")
    def GetServerInformation(self):
        return ("mock", "telamon", "1", "1.2")

    @dbus.service.method("org.freedesktop.Notifications", in_signature="u", out_signature="")
    def CloseNotification(self, nid):
        self.NotificationClosed(nid, 3)

    @dbus.service.method("org.freedesktop.Notifications", in_signature="susssasa{sv}i", out_signature="u")
    def Notify(self, app, replaces, icon, summary, body, actions, hints, timeout):
        nid = self.next_id
        self.next_id += 1
        h = {}
        for k, v in hints.items():
            if k == "image-data":
                w, hh, rs, alpha, bits, ch, data = v
                h[str(k)] = {"w": int(w), "h": int(hh), "rowstride": int(rs), "alpha": bool(alpha),
                             "bits": int(bits), "channels": int(ch), "bytes": len(data)}
            else:
                h[str(k)] = plain(v)
        log({"kind": "notify", "id": nid, "app": str(app), "icon": str(icon), "summary": str(summary),
             "body": str(body), "actions": plain(actions), "hints": h, "timeout": int(timeout)})
        self.last = nid
        return dbus.UInt32(nid)

    def poll(self):
        try:
            key = open(ACTION_FILE).read().strip()
        except OSError:
            key = ""
        if key and self.last is not None:
            open(ACTION_FILE, "w").close()
            self.click(self.last, key)
        return True

    def click(self, nid, key):
        self.ActionInvoked(nid, key)
        GLib.timeout_add(100, lambda: (self.NotificationClosed(nid, 2), False)[1])
        return False

    @dbus.service.signal("org.freedesktop.Notifications", signature="us")
    def ActionInvoked(self, nid, key):
        pass

    @dbus.service.signal("org.freedesktop.Notifications", signature="uu")
    def NotificationClosed(self, nid, reason):
        pass


class FileManager(dbus.service.Object):
    def __init__(self, bus):
        super().__init__(bus, "/org/freedesktop/FileManager1")

    @dbus.service.method("org.freedesktop.FileManager1", in_signature="ass", out_signature="")
    def ShowItems(self, uris, startup_id):
        log({"kind": "show-items", "uris": plain(uris), "startup_id": str(startup_id)})


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    names = [dbus.service.BusName("org.freedesktop.Notifications", bus),
             dbus.service.BusName("org.freedesktop.FileManager1", bus)]
    keep = [Notifications(bus), FileManager(bus)]
    open(LOG, "a").close()
    GLib.MainLoop().run()


main()
