import io
from pathlib import Path
import shlex
import subprocess
import struct
import sys
import tempfile
import unittest
import zlib
from unittest.mock import patch
from test_runtime import g, h, module, frame

c = module('clipboard')


class ChatUploads(unittest.TestCase):
    def test_upload_completes_before_literal_reference_is_pasted(self):
        with tempfile.TemporaryDirectory() as root, patch.object(g,'WORK',Path(root)):
            path = "inputs/a ' $(touch BAD).txt"
            calls = []
            def run(argv, **kwargs):
                calls.append((argv,kwargs))
                if 'display-message' in argv:
                    return subprocess.CompletedProcess(argv,0,b'%0\t42\t0\t0\n')
                if 'load-buffer' in argv:
                    self.assertEqual((Path(root)/path).read_bytes(), b'content\n')
                    text=kwargs['input'].decode()
                    self.assertEqual(shlex.split(text), [str(Path(root)/path)])
                    self.assertNotIn('\n',text)
                    self.assertNotIn('\x1b',text)
                return subprocess.CompletedProcess(argv,0,b'')
            with patch.object(g.subprocess,'run',side_effect=run):
                g.put(path,io.BytesIO(frame(b'content\n')),chat='vt')
            self.assertEqual(sum('paste-buffer' in args for args,_ in calls),1)
            self.assertTrue(any('paste-buffer' in args and '-p' in args for args,_ in calls))
            self.assertFalse(any('send-keys' in args for args,_ in calls))

    def test_bad_frame_or_collision_never_inserts(self):
        with tempfile.TemporaryDirectory() as root, patch.object(g,'WORK',Path(root)), \
             patch.object(g,'chat_pane',return_value=([],['%0','42'])), patch.object(g,'insert_reference') as insert:
            for data in (frame(b'abc')[:-1],frame(b'abc')+b'extra'):
                with self.assertRaises(ValueError):g.put('image',io.BytesIO(data),chat='vt')
                self.assertFalse((Path(root)/'image').exists())
            (Path(root)/'image').write_text('original')
            with self.assertRaises(FileExistsError):g.put('image',io.BytesIO(frame(b'new')),chat='vt')
            self.assertEqual((Path(root)/'image').read_text(),'original')
            insert.assert_not_called()

    def test_missing_chat_consumes_nothing_and_late_failure_keeps_file(self):
        with tempfile.TemporaryDirectory() as root, patch.object(g,'WORK',Path(root)):
            stream=io.BytesIO(frame(b'data'))
            with patch.object(g,'chat_pane',side_effect=ValueError('not ready')):
                with self.assertRaises(ValueError):g.put('file',stream,chat='vt')
            self.assertEqual(stream.tell(),0)
            self.assertFalse((Path(root)/'file').exists())
            with patch.object(g,'chat_pane',side_effect=[([],['%0','42']),([],['%0','99'])]):
                with self.assertRaisesRegex(RuntimeError,'file remains available'):
                    g.put('file',stream,chat='vt')
            self.assertEqual((Path(root)/'file').read_bytes(),b'data')

    def test_host_enters_guest_as_agent_before_pasting(self):
        class Entered(Exception):pass
        with patch.object(h.sys,'platform','linux'),patch.object(h.os,'geteuid',return_value=0), \
             patch.object(h,'load',return_value={'name':'demo'}),patch.object(h,'guard'), \
             patch.object(h.os,'execv',side_effect=Entered) as execute:
            with self.assertRaises(Entered):h.dispatch(['put','demo','--chat','vt','--replace','--','a.txt'])
            command=execute.call_args.args[1]
            self.assertIn('--uid=agent',command)
            self.assertEqual(command[-4:],['put','replace','a.txt','vt'])


class Clipboard(unittest.TestCase):
    def test_image_priority_and_unsupported_types(self):
        self.assertEqual(c.choose_type(['text/plain','image/png']),('image/png','png'))
        self.assertEqual(c.choose_type(['image/jpeg']),('image/jpeg','jpg'))
        with self.assertRaises(ValueError):c.choose_type(['application/pdf'])

    def test_linux_text_preserves_exact_bytes_and_no_implicit_newline(self):
        with tempfile.TemporaryDirectory() as root,patch.object(c.sys,'platform','linux'), \
             patch.dict(c.os.environ,{'WAYLAND_DISPLAY':'test'},clear=True), \
             patch.object(c.shutil,'which',return_value='/bin/wl-paste'), \
             patch.object(c.subprocess,'check_output',return_value=b'text/plain\n'):
            def run(argv, **kwargs):
                self.assertIn('--no-newline',argv)
                kwargs['stdout'].write(b'line one\nline two')
            with patch.object(c.subprocess,'run',side_effect=run):
                self.assertEqual(c.capture(Path(root)),{'format':'txt'})
            self.assertEqual((Path(root)/'clipboard.txt').read_bytes(),b'line one\nline two')

    @unittest.skipUnless(sys.platform=='darwin','native macOS pasteboard')
    def test_native_mac_item_text_and_image(self):
        # A real AppKit item tests the native bridge without a desktop session
        # or touching the user's clipboard (also works on headless macOS CI).
        def chunk(kind,data):
            return struct.pack('!I',len(data))+kind+data+struct.pack('!I',zlib.crc32(kind+data))
        png = (b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!IIBBBBB',1,1,8,2,0,0,0))
               +chunk(b'IDAT',zlib.compress(b'\x00\xff\x00\x00'))+chunk(b'IEND',b''))
        native=c.MacPasteboard()
        try:
            for kind,value in [('txt','first\nsecond ✓'),('png',png),('tiff',png)]:
                pb=native.call(native.call(native.cls('NSPasteboardItem'),'alloc'),'init')
                if kind=='txt':
                    native.call(pb,'setString:forType:',native.string(value),native.string('public.utf8-plain-text'))
                    expected=value.encode()
                else:
                    expected=png
                    data=native.call(native.cls('NSData'),'dataWithBytes:length:',expected,len(expected),
                                     types=[native.C.c_char_p,native.C.c_size_t])
                    if kind=='tiff':
                        bitmap=native.call(native.cls('NSBitmapImageRep'),'imageRepWithData:',data)
                        self.assertTrue(bitmap)
                        data=native.call(bitmap,'TIFFRepresentation')
                    native.call(pb,'setData:forType:',data,native.string('public.'+kind))
                actual=native.read(pb)
                if kind=='tiff':
                    self.assertEqual(actual[0],'png')
                    self.assertTrue(actual[1].startswith(b'\x89PNG\r\n\x1a\n'))
                else:self.assertEqual(actual,(kind,expected))
                native.call(pb,'release')
        finally:
            native.close()
