#!/usr/bin/env python3
"""Native login/app-server fixture. No external account, token or model calls."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import sys
import threading
from urllib.parse import parse_qs, urlsplit

home = Path(os.environ['CODEX_HOME']) if 'CODEX_HOME' in os.environ else Path.home()/'.codex'
if '--version' in sys.argv:
    print('codex-cli 0.165.0')
elif 'status' in sys.argv:
    if (home/'auth.json').exists():
        print('Logged in using ChatGPT')
    else:
        sys.exit(1)
elif 'app-server' in sys.argv:
    server=None
    class Callback(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            parsed=urlsplit(self.path)
            if parsed.path=='/auth/callback':
                assert parse_qs(parsed.query)=={'code':['private-authorization-code'],'state':['fixture-state']}
                (home/'auth.json').write_text('{"test_credential":"private-native-token"}')
                self.send_response(302)
                self.send_header('Location','http://127.0.0.1:1455/success')
                self.end_headers()
            elif parsed.path=='/success':
                self.send_response(200);self.end_headers();self.wfile.write(b'You are signed in')
                print(json.dumps({'method':'account/login/completed','params':{'loginId':'fixture-login','success':True}}),flush=True)
            else:raise AssertionError('unexpected callback path')
    try:
        for line in sys.stdin:
            message=json.loads(line)
            if message['method']=='initialize':
                result={}
            elif message['method']=='initialized':
                continue
            elif message['method']=='account/read':
                result={'account':{'type':'chatgpt'},'workspaceRouting':{'backendOrigin':'https://chatgpt.com'}}
            elif message['method']=='account/login/start':
                server=ThreadingHTTPServer(('127.0.0.1',0),Callback)
                (home/'.fixture-callback-port').write_text(str(server.server_address[1]))
                threading.Thread(target=server.serve_forever,daemon=True).start()
                result={'type':'chatgpt','loginId':'fixture-login',
                        'authUrl':'https://auth.openai.com/oauth/authorize?state=fixture-state&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback'}
            else:
                raise AssertionError('unexpected native call')
            print(json.dumps({'id':message['id'],'result':result}),flush=True)
    finally:
        if server:
            server.shutdown();server.server_close()
            (home/'.fixture-callback-closed').touch()
else:
    raise AssertionError('unexpected native invocation')
