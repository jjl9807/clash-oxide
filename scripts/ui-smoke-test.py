#!/usr/bin/env python3
"""Exercise real TUI keyboard/mouse input and GUI input on an isolated Xvfb."""
import ctypes
import importlib.util
from pathlib import Path

def close_window(display_name, window):
    # Send WM_DELETE_WINDOW directly; Xvfb intentionally has no window manager.
    class Data(ctypes.Union): _fields_ = [('l', ctypes.c_long * 5)]
    class Message(ctypes.Structure):
        _fields_ = [('type', ctypes.c_int), ('serial', ctypes.c_ulong), ('send_event', ctypes.c_int), ('display', ctypes.c_void_p), ('window', ctypes.c_ulong), ('message_type', ctypes.c_ulong), ('format', ctypes.c_int), ('data', Data)]
    class Event(ctypes.Union): _fields_ = [('message', Message), ('pad', ctypes.c_long * 24)]
    xlib = ctypes.CDLL('libX11.so.6')
    xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]; xlib.XOpenDisplay.restype = ctypes.c_void_p
    xlib.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]; xlib.XInternAtom.restype = ctypes.c_ulong
    xlib.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(Event)]
    xlib.XFlush.argtypes = [ctypes.c_void_p]; xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = xlib.XOpenDisplay(display_name.encode()); assert display
    event = Event(); event.message.type = 33; event.message.display = display
    event.message.window = int(window); event.message.format = 32
    event.message.message_type = xlib.XInternAtom(display, b'WM_PROTOCOLS', 0)
    event.message.data.l[0] = xlib.XInternAtom(display, b'WM_DELETE_WINDOW', 0)
    assert xlib.XSendEvent(display, int(window), 0, 0, ctypes.byref(event))
    xlib.XFlush(display); xlib.XCloseDisplay(display)

def main():
    # The native workflow test covers the redesigned screens and keyboard model.
    spec = importlib.util.spec_from_file_location('party', Path(__file__).with_name('ui-party-test.py'))
    party = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(party)
    party.main()

if __name__ == '__main__': main()
