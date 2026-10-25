"""Patch pywebview's Windows backend so it imports on .NET Core (CoreCLR).

Why this file exists
--------------------
`webview/platforms/winforms.py` builds its folder picker at *class definition*
time, by reflecting into `System.Windows.Forms` from `pythonnet`:

    class OpenFolderDialog:
        flags = ...
        windowsFormsAssembly = ...
        iFileDialogType = windowsFormsAssembly.GetType(
            'System.Windows.Forms.FileDialogNative+IFileDialog')
        setOptionsMethodInfo = iFileDialogType.GetMethod('SetOptions', flags)

On .NET Framework 4.8 that nested type exists. On .NET Core / .NET 10 it does not,
so `GetType` returns None and `None.GetMethod` raises AttributeError during import.
pywebview's `guilib.try_import` only catches ImportError, so the AttributeError
becomes the message "You must have pythonnet installed in order to use pywebview" --
which sends you looking at pythonnet, the wrong place.

This application never opens a folder picker, so the fix is to let that block fail
quietly and make `show()` complain only if something actually asks for it. It is a
patch to an installed third-party file, so it is written down here, idempotent,
and leaves a `.bak` behind.

What it does NOT do: nothing else in pywebview is touched, and this does not change
any behaviour of the window, the page, or the JS bridge.

Usage:
    python tools\\fix_pywebview.py            # patch (safe to re-run)
    python tools\\fix_pywebview.py --revert   # put the backup back
"""

import io
import os
import sys

MARK = "CORE-CLR PATCH"

OLD_BLOCK = """class OpenFolderDialog:
    foldersFilter = 'Folders|\\n'
    flags = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic
    windowsFormsAssembly = Assembly.LoadWithPartialName('System.Windows.Forms')
    iFileDialogType = windowsFormsAssembly.GetType(
        'System.Windows.Forms.FileDialogNative+IFileDialog'
    )
    OpenFileDialogType = windowsFormsAssembly.GetType('System.Windows.Forms.OpenFileDialog')
    FileDialogType = windowsFormsAssembly.GetType('System.Windows.Forms.FileDialog')
    createVistaDialogMethodInfo = OpenFileDialogType.GetMethod('CreateVistaDialog', flags)
    onBeforeVistaDialogMethodInfo = OpenFileDialogType.GetMethod('OnBeforeVistaDialog', flags)
    getOptionsMethodInfo = FileDialogType.GetMethod('GetOptions', flags)
    setOptionsMethodInfo = iFileDialogType.GetMethod('SetOptions', flags)
    fosPickFoldersBitFlag = (
        windowsFormsAssembly.GetType('System.Windows.Forms.FileDialogNative+FOS')
        .GetField('FOS_PICKFOLDERS')
        .GetValue(None)
    )

    vistaDialogEventsConstructorInfo = windowsFormsAssembly.GetType(
        'System.Windows.Forms.FileDialog+VistaDialogEvents'
    ).GetConstructor(flags, None, [FileDialogType], [])
    adviseMethodInfo = iFileDialogType.GetMethod('Advise')
    unadviseMethodInfo = iFileDialogType.GetMethod('Unadvise')
    showMethodInfo = iFileDialogType.GetMethod('Show')
"""

NEW_BLOCK = """class OpenFolderDialog:
    foldersFilter = 'Folders|\\n'
    flags = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic
    windowsFormsAssembly = Assembly.LoadWithPartialName('System.Windows.Forms')
    # CORE-CLR PATCH (PC-Monitor, 2026-10-25): .NET Core WinForms does not expose
    # System.Windows.Forms.FileDialogNative+IFileDialog, so this block -- which runs
    # while the class body executes -- raised AttributeError and made the whole
    # backend unimportable. Nothing else in this file depends on the picker: if the
    # reflection fails, the members stay None and show() raises a clear error.
    try:
        iFileDialogType = windowsFormsAssembly.GetType(
            'System.Windows.Forms.FileDialogNative+IFileDialog'
        )
        OpenFileDialogType = windowsFormsAssembly.GetType('System.Windows.Forms.OpenFileDialog')
        FileDialogType = windowsFormsAssembly.GetType('System.Windows.Forms.FileDialog')
        createVistaDialogMethodInfo = OpenFileDialogType.GetMethod('CreateVistaDialog', flags)
        onBeforeVistaDialogMethodInfo = OpenFileDialogType.GetMethod('OnBeforeVistaDialog', flags)
        getOptionsMethodInfo = FileDialogType.GetMethod('GetOptions', flags)
        setOptionsMethodInfo = iFileDialogType.GetMethod('SetOptions', flags)
        fosPickFoldersBitFlag = (
            windowsFormsAssembly.GetType('System.Windows.Forms.FileDialogNative+FOS')
            .GetField('FOS_PICKFOLDERS')
            .GetValue(None)
        )

        vistaDialogEventsConstructorInfo = windowsFormsAssembly.GetType(
            'System.Windows.Forms.FileDialog+VistaDialogEvents'
        ).GetConstructor(flags, None, [FileDialogType], [])
        adviseMethodInfo = iFileDialogType.GetMethod('Advise')
        unadviseMethodInfo = iFileDialogType.GetMethod('Unadvise')
        showMethodInfo = iFileDialogType.GetMethod('Show')
    except Exception:  # noqa: BLE001 - any reflection failure means "no picker"
        iFileDialogType = None
        vistaDialogEventsConstructorInfo = None
        adviseMethodInfo = None
        unadviseMethodInfo = None
        showMethodInfo = None
"""

GUARD_OLD = """    def show(cls, parent=None, initialDirectory=None, allow_multiple=False, title=None):
        openFileDialog = WinForms.OpenFileDialog()
"""

GUARD_NEW = """    def show(cls, parent=None, initialDirectory=None, allow_multiple=False, title=None):
        if cls.showMethodInfo is None:
            raise RuntimeError(
                'OpenFolderDialog needs the .NET Framework FileDialogNative interface, '
                'which this runtime does not expose'
            )

        openFileDialog = WinForms.OpenFileDialog()
"""


def target():
    import webview.platforms.winforms as winforms

    return os.path.abspath(winforms.__file__)


def main(argv):
    path = target()
    text = io.open(path, encoding="utf-8").read()
    backup = path + ".bak"

    if "--revert" in argv:
        if not os.path.exists(backup):
            print("no backup to restore: %s" % backup)
            return 1
        io.open(path, "w", encoding="utf-8", newline="\n").write(
            io.open(backup, encoding="utf-8").read())
        print("reverted %s" % path)
        return 0

    if MARK in text:
        print("already patched: %s" % path)
        return 0

    if OLD_BLOCK not in text:
        print("PATCH FAILED: the block to replace was not found verbatim in %s" % path)
        print("pywebview version changed? re-read the file before trusting this script.")
        return 2

    if GUARD_OLD not in text:
        print("PATCH FAILED: show() was not found verbatim in %s" % path)
        return 2

    patched = text.replace(OLD_BLOCK, NEW_BLOCK, 1).replace(GUARD_OLD, GUARD_NEW, 1)
    io.open(backup, "w", encoding="utf-8", newline="\n").write(text)
    io.open(path, "w", encoding="utf-8", newline="\n").write(patched)
    print("patched %s (backup: %s)" % (path, backup))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
