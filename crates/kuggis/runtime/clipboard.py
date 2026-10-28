"""Local clipboard only. Captured bytes never appear in diagnostics or stdout."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

class MacPasteboard:
    """Small, typed Objective-C bridge using Python's standard library."""
    def __init__(self):
        import ctypes as C
        self.C = C
        self.appkit = C.CDLL('/System/Library/Frameworks/AppKit.framework/AppKit')
        self.objc = C.CDLL('/usr/lib/libobjc.A.dylib')
        self.objc.objc_getClass.argtypes = [C.c_char_p]
        self.objc.objc_getClass.restype = C.c_void_p
        self.objc.sel_registerName.argtypes = [C.c_char_p]
        self.objc.sel_registerName.restype = C.c_void_p
        self.pool = self.call(self.call(self.cls('NSAutoreleasePool'),'alloc'),'init')

    def cls(self, name):
        return self.objc.objc_getClass(name.encode())

    def call(self, receiver, selector, *args, result=None, types=None):
        C = self.C
        signature = C.CFUNCTYPE(result or C.c_void_p, C.c_void_p, C.c_void_p,
                               *(types or [C.c_void_p]*len(args)))
        fn = signature(('objc_msgSend', self.objc))
        return fn(receiver, self.objc.sel_registerName(selector.encode()), *args)

    def string(self, value):
        return self.call(self.cls('NSString'),'stringWithUTF8String:',value.encode(),
                         types=[self.C.c_char_p])

    def read(self, pasteboard=None):
        C = self.C
        pb = pasteboard or self.call(self.cls('NSPasteboard'),'generalPasteboard')
        if not pb:
            raise RuntimeError('macOS clipboard is unavailable; run --paste from your local desktop terminal')
        data = self.call(pb,'dataForType:',self.string('public.png'))
        ext = 'png'
        if not data:
            tiff = self.call(pb,'dataForType:',self.string('public.tiff'))
            if tiff:
                bitmap = self.call(self.cls('NSBitmapImageRep'),'imageRepWithData:',tiff)
                data = self.call(bitmap,'representationUsingType:properties:',4,
                                 self.call(self.cls('NSDictionary'),'dictionary'),
                                 types=[C.c_ulong,C.c_void_p])
        if not data:
            data = self.call(pb,'dataForType:',self.string('public.jpeg'))
            ext = 'jpg'
        if not data:
            text = self.call(pb,'stringForType:',self.string('public.utf8-plain-text'))
            data = self.call(text,'dataUsingEncoding:',4,types=[C.c_ulong]) if text else None
            ext = 'txt'
        if not data:
            raise ValueError('clipboard has no supported image or plain text')
        size = self.call(data,'length',result=C.c_size_t)
        if size > 64*1024*1024:
            raise ValueError('clipboard capture exceeds 64 MiB; save it and upload as a file')
        return ext, C.string_at(self.call(data,'bytes'),size)

    def close(self):
        self.call(self.pool,'drain')


def choose_type(types):
    for mime, ext in [('image/png','png'), ('image/jpeg','jpg'),
                      ('text/plain;charset=utf-8','txt'), ('text/plain','txt'), ('UTF8_STRING','txt')]:
        if mime in types:
            return mime, ext
    raise ValueError('clipboard has no supported image or plain text')


def capture(directory):
    if sys.platform == 'darwin':
        native = MacPasteboard()
        try:
            ext, data = native.read()
            (directory/('clipboard.'+ext)).write_bytes(data)
        finally:
            native.close()
    elif sys.platform == 'linux':
        if os.environ.get('WAYLAND_DISPLAY') and shutil.which('wl-paste'):
            base = ['wl-paste']
            types = subprocess.check_output([*base,'--list-types'],timeout=10).decode().splitlines()
            mime, ext = choose_type(types)
            command = [*base,'--no-newline','--type',mime]
        elif os.environ.get('DISPLAY') and shutil.which('xclip'):
            base = ['xclip','-selection','clipboard','-o']
            types = subprocess.check_output([*base,'-t','TARGETS'],timeout=10).decode().splitlines()
            mime, ext = choose_type(types)
            command = [*base,'-t',mime]
        else:
            raise RuntimeError('--paste requires a local desktop with wl-paste (Wayland) or xclip (X11)')
        with (directory/('clipboard.'+ext)).open('xb') as output:
            subprocess.run(command,stdout=output,stderr=subprocess.PIPE,check=True,timeout=20)
    else:
        raise RuntimeError('--paste currently supports macOS and Linux; use a file on this platform')
    if ext not in ('png','jpg','txt'):
        raise ValueError('unexpected clipboard format')
    path = directory/('clipboard.'+ext)
    path.chmod(0o600)
    if path.stat().st_size == 0:
        raise ValueError('clipboard is empty')
    return {'format':ext}


if __name__ == '__main__':
    os.umask(0o077)
    try:
        print(json.dumps(capture(Path(sys.argv[1]))))
    except Exception as error:
        # Do not echo clipboard data, external command output, or command argv.
        message = str(error) if isinstance(error,(ValueError,RuntimeError)) else type(error).__name__
        print('kuggis clipboard: '+message,file=sys.stderr)
        sys.exit(1)
