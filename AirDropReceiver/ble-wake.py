#!/usr/bin/python
"""Temporarily announce an anonymous AirDrop sender through BlueZ."""
import signal
import sys
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

ADVERTISEMENT = "org.bluez.LEAdvertisement1"
PATH = "/org/codex/AirDropWake"


class WakeAdvertisement(dbus.service.Object):
    @dbus.service.method("org.freedesktop.DBus.Properties", in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        if interface != ADVERTISEMENT:
            return {}
        # AirDrop TLV: type 5, length 18, version 1. Anonymous contact hashes;
        # requires Everyone mode. No Apple account or phone data is included.
        payload = bytes([5, 18]) + bytes(8) + bytes([1]) + bytes.fromhex("e3b0" * 4) + bytes([0])
        return {"Type": "broadcast", "Timeout": dbus.UInt16(300),
                "MinInterval": dbus.UInt32(200), "MaxInterval": dbus.UInt32(200),
                "ManufacturerData": dbus.Dictionary(
                    {dbus.UInt16(0x004c): dbus.Array(payload, signature="y")}, signature="qv")}

    @dbus.service.method(ADVERTISEMENT, in_signature="", out_signature="")
    def Release(self):
        loop.quit()


def main():
    global loop
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
    objects = bus.get_object("org.bluez", "/").GetManagedObjects(
        dbus_interface="org.freedesktop.DBus.ObjectManager")
    adapter = next((path for path, interfaces in objects.items()
                    if "org.bluez.LEAdvertisingManager1" in interfaces
                    and interfaces.get("org.bluez.Adapter1", {}).get("Powered")), None)
    if adapter is None:
        print("Enable Bluetooth to discover the iPhone.", file=sys.stderr)
        return 1
    manager = dbus.Interface(bus.get_object("org.bluez", adapter), "org.bluez.LEAdvertisingManager1")
    advertisement = WakeAdvertisement(bus, PATH)
    loop = GLib.MainLoop()
    failed = []
    registered = []
    def ready():
        registered.append(True)
        print("Bluetooth AirDrop wake-up active", flush=True)
    def error(err):
        failed.append(err)
        print(str(err), file=sys.stderr, flush=True)
        loop.quit()
    manager.RegisterAdvertisement(PATH, {}, reply_handler=ready, error_handler=error)
    signal.signal(signal.SIGTERM, lambda *_: loop.quit())
    signal.signal(signal.SIGINT, lambda *_: loop.quit())
    GLib.timeout_add_seconds(300, lambda: (loop.quit(), False)[1])
    try:
        loop.run()
    finally:
        if registered:
            try:
                manager.UnregisterAdvertisement(PATH)
            except dbus.DBusException:
                pass
        advertisement.remove_from_connection()
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
