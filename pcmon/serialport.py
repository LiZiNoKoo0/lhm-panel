"""Minimal read-only serial port for Windows, via ctypes -- no pyserial.

Only what the firmware parity check needs: open a COM port at a fixed baud,
pulse the board's auto-reset lines, read bytes until a deadline. Deliberately
tiny: this repo ships without pip dependencies, and installing pyserial on a
bench machine just to read 400 bytes is not worth a dependency.

Two things this file learned the hard way on the desk board (CH343, ESP32):

  * DTR is wired to IO0 and RTS to EN. Asserting *both* holds IO0 low while the
    chip comes out of reset, i.e. it silently drops into the ROM download mode
    and prints nothing. Only RTS may be pulsed; DTR must stay deasserted.
  * DCB.fDtrControl/fRtsControl must be ENABLE (1), otherwise the driver
    overrides the line state and EscapeCommFunction has no visible effect.

Not a general serial library: no parity/flow-control options, no writes.
"""
import ctypes
import ctypes.wintypes as wt
import time

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
OPEN_EXISTING = 3
INVALID_HANDLE_VALUE = wt.HANDLE(-1).value

SETRTS, CLRRTS, SETDTR, CLRDTR = 3, 4, 5, 6
PURGE_RXCLEAR, PURGE_TXCLEAR = 0x0008, 0x0004

k32 = ctypes.WinDLL("kernel32", use_last_error=True)


class DCB(ctypes.Structure):
    _fields_ = [("DCBlength", wt.DWORD), ("BaudRate", wt.DWORD),
                ("fBinary", wt.DWORD, 1), ("fParity", wt.DWORD, 1),
                ("fOutxCtsFlow", wt.DWORD, 1), ("fOutxDsrFlow", wt.DWORD, 1),
                ("fDtrControl", wt.DWORD, 2), ("fDsrSensitivity", wt.DWORD, 1),
                ("fTXContinueOnXoff", wt.DWORD, 1), ("fOutX", wt.DWORD, 1),
                ("fInX", wt.DWORD, 1), ("fErrorChar", wt.DWORD, 1),
                ("fNull", wt.DWORD, 1), ("fRtsControl", wt.DWORD, 2),
                ("fAbortOnError", wt.DWORD, 1), ("fDummy2", wt.DWORD, 17),
                ("wReserved", wt.WORD), ("XonLim", wt.WORD), ("XoffLim", wt.WORD),
                ("ByteSize", wt.BYTE), ("Parity", wt.BYTE), ("StopBits", wt.BYTE),
                ("XonChar", ctypes.c_char), ("XoffChar", ctypes.c_char),
                ("ErrorChar", ctypes.c_char), ("EofChar", ctypes.c_char),
                ("EvtChar", ctypes.c_char), ("wReserved1", wt.WORD)]


class COMMTIMEOUTS(ctypes.Structure):
    _fields_ = [("ReadIntervalTimeout", wt.DWORD),
                ("ReadTotalTimeoutMultiplier", wt.DWORD),
                ("ReadTotalTimeoutConstant", wt.DWORD),
                ("WriteTotalTimeoutMultiplier", wt.DWORD),
                ("WriteTotalTimeoutConstant", wt.DWORD)]


class COMSTAT(ctypes.Structure):
    _fields_ = [("fCtsHold", wt.DWORD, 1), ("fDsrHold", wt.DWORD, 1),
                ("fRlsdHold", wt.DWORD, 1), ("fXoffHold", wt.DWORD, 1),
                ("fXoffSent", wt.DWORD, 1), ("fEof", wt.DWORD, 1),
                ("fTxim", wt.DWORD, 1), ("fReserved", wt.DWORD, 25),
                ("cbInQue", wt.DWORD), ("cbOutQue", wt.DWORD)]


class Port(object):
    """A blocking COM port you can only read from."""

    def __init__(self, name, baud=115200):
        self.handle = k32.CreateFileW(r"\\.\%s" % name, GENERIC_READ | GENERIC_WRITE,
                                      3, None, OPEN_EXISTING, 0, None)
        if self.handle == INVALID_HANDLE_VALUE:
            raise OSError(ctypes.get_last_error(), "cannot open %s" % name)
        self._status = COMSTAT()
        self._errors = wt.DWORD(0)
        dcb = DCB()
        dcb.DCBlength = ctypes.sizeof(DCB)
        if not k32.GetCommState(self.handle, ctypes.byref(dcb)):
            self.close()
            raise OSError(ctypes.get_last_error(), "GetCommState failed")
        dcb.BaudRate = baud
        dcb.ByteSize = 8
        dcb.Parity = 0          # NOPARITY
        dcb.StopBits = 0        # ONESTOPBIT
        dcb.fBinary = 1
        dcb.fParity = 0
        dcb.fOutxCtsFlow = 0
        dcb.fOutxDsrFlow = 0
        dcb.fDtrControl = 1     # ENABLE -- else SetDTR/ClearDTR are overridden
        dcb.fRtsControl = 1     # ENABLE -- else SetRTS/ClearRTS are overridden
        dcb.fOutX = 0
        dcb.fInX = 0
        dcb.fAbortOnError = 0
        if not k32.SetCommState(self.handle, ctypes.byref(dcb)):
            self.close()
            raise OSError(ctypes.get_last_error(), "SetCommState failed")
        k32.SetupComm(self.handle, 65536, 65536)
        timeouts = COMMTIMEOUTS(50, 0, 100, 0, 100)
        k32.SetCommTimeouts(self.handle, ctypes.byref(timeouts))

    def reset_board(self, hold_s=0.3):
        """Restart the app: RTS is EN, DTR is IO0.

        DTR stays deasserted so the chip boots the flashed app instead of the
        ROM download stub. Stale bytes from an earlier session are dropped
        first, otherwise they interleave with the fresh boot log.
        """
        k32.PurgeComm(self.handle, PURGE_RXCLEAR | PURGE_TXCLEAR)
        k32.EscapeCommFunction(self.handle, CLRDTR)
        k32.EscapeCommFunction(self.handle, SETRTS)
        time.sleep(hold_s)
        k32.EscapeCommFunction(self.handle, CLRRTS)

    def queued(self):
        """Bytes waiting in the driver's receive buffer."""
        if not k32.ClearCommError(self.handle, ctypes.byref(self._errors),
                                  ctypes.byref(self._status)):
            return 0
        return self._status.cbInQue

    def read(self, size=4096):
        if not self.queued():
            return b""
        buf = ctypes.create_string_buffer(size)
        got = wt.DWORD(0)
        if not k32.ReadFile(self.handle, buf, size, ctypes.byref(got), None):
            return b""
        return buf.raw[:got.value]

    def close(self):
        if getattr(self, "handle", None) and self.handle != INVALID_HANDLE_VALUE:
            k32.CloseHandle(self.handle)
            self.handle = INVALID_HANDLE_VALUE


def read_for(name, baud=115200, seconds=10.0, reset=True, until=None, poll=0.02,
             tries=4, window=2.5):
    """Collect text from a COM port until `until(text)` or the deadline.

    The EN pulse is not 100% reliable on the CH343 board on this desk: roughly
    one attempt in three is ignored and the app never restarts, so nothing is
    printed. The app only prints at boot, so a missed pulse looks exactly like
    a dead board. Hence: re-pulse, up to `tries` times, while the deadline
    allows, and keep whatever text each attempt produced.
    """
    port = Port(name, baud)
    try:
        text = ""
        deadline = time.time() + seconds
        for attempt in range(tries):
            if attempt:
                time.sleep(0.4)
            if reset:
                port.reset_board()
            window_end = min(deadline, time.time() + window)
            while time.time() < window_end:
                chunk = port.read()
                if chunk:
                    text += chunk.decode("utf-8", "replace")
                    if until is not None and until(text):
                        return text
                else:
                    time.sleep(poll)
            if time.time() >= deadline:
                break
        return text
    finally:
        port.close()
