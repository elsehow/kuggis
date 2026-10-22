#!/usr/bin/env python3
"""Opt-in real HTTP/browser test of the operator Resources UI.
Uses disposable local registry/key and an already installed Playwright browser.
Usage: PUBLISH_SMOKE_PLAYWRIGHT=/path/to/playwright python3 scripts/test-resource-ui.py [cell-binary]
"""
import hashlib, json, os, socket, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path
root=Path(__file__).resolve().parent.parent
binary=Path(sys.argv[1] if len(sys.argv)>1 else root/'cell/target/debug/cell').resolve()
playwright=os.environ.get('PUBLISH_SMOKE_PLAYWRIGHT')
if not playwright: sys.exit('Set PUBLISH_SMOKE_PLAYWRIGHT to an installed Playwright module (no downloads).')
processes=[]
def run(args,stdin=None):
 p=subprocess.run([str(x) for x in args],input=stdin,capture_output=True,text=True)
 if p.returncode: raise RuntimeError(f'{args}\n{p.stderr}\n{p.stdout}')
 return p.stdout.strip()
def port():
 with socket.socket() as s: s.bind(('127.0.0.1',0)); return s.getsockname()[1]
def launch(args,log,at):
 with open(log,'w') as stream: p=subprocess.Popen([str(x) for x in args],stdout=stream,stderr=stream)
 processes.append(p)
 for _ in range(100):
  if p.poll() is not None: raise RuntimeError(Path(log).read_text())
  try: socket.create_connection(('127.0.0.1',at),timeout=.1).close(); return
  except OSError: time.sleep(.05)
 raise RuntimeError('server startup timed out')
def post(url,body):
 req=urllib.request.Request(url,data=json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
 return json.load(opener.open(req))
try:
 with tempfile.TemporaryDirectory(prefix='resource-ui-') as tmp:
  tmp=Path(tmp);tree=tmp/'tree';(tree/'.pins').mkdir(parents=True);(tmp/'sockets').mkdir()
  public=run([binary,'key','new',tmp/'key']);registry=tmp/'registry'
  audience=run([binary,'publisher','init',registry]);ns=run([binary,'publisher','namespace',registry,public])
  (tree/'author.json').write_text(json.dumps({'name':'author','grants':[{'publish':{'namespace':ns}}]}));(tree/'.pins/author.pub').write_text(public)
  run([binary,'publisher','configure',registry,'--public-base','https://sites.example.com'])
  p=subprocess.run([str(binary),'publisher','plan',str(registry),str(tree)],capture_output=True,text=True,check=True)
  run([binary,'publisher','apply',registry,tree,'--approve',p.stderr.strip().removeprefix('approve with ')])
  artifact={'files':[{'path':'index.html','bytes':list(b'hello')}]};digest=hashlib.sha256(json.dumps(artifact,separators=(',',':')).encode()).hexdigest()
  signed=json.loads(run([binary,'publisher','sign','--secret',tmp/'key','--audience',audience],json.dumps({'publish':{'site':{'namespace':ns,'name':'demo'},'artifact':digest}})))
  control=port();launch([binary,'publisher','serve',registry,'--listen',f'127.0.0.1:{control}'],tmp/'control.log',control)
  post(f'http://127.0.0.1:{control}/v1/uploads',{'request':signed,'artifact':artifact})
  config=tree/'.resources.json';config.write_text(json.dumps({'backends':[{'name':'sites','provider':audience,'location':{'kind':'local','registry':str(registry)}}]}))
  # Operator resources must remain usable without any original cell declaration.
  (tree/'author.json').unlink();(tree/'.pins/author.pub').unlink()
  ui=port();launch([binary,'ui',tree,'--state',tmp/'operator','--socket-dir',tmp/'sockets','--assets',root/'web/dist','--listen',f'127.0.0.1:{ui}'],tmp/'ui.log',ui)
  script=r'''
const {chromium}=require(process.argv[1]); const fs=require('fs');
(async()=>{const browser=await chromium.launch({headless:true});try{
 const page=await browser.newPage(); await page.goto(process.argv[2]+'/?view=resources');
 await page.getByRole('heading',{name:'Resources',exact:true}).waitFor();
 await page.getByText(/Former\/unavailable cell/).first().waitFor();
 await page.getByRole('button',{name:'Unpublish site…'}).click();
 await page.getByRole('heading',{name:'Review resource operation'}).waitFor();
 // Retargeting config between review/apply must not redirect the operation.
 const original=fs.readFileSync(process.argv[3],'utf8');const config=JSON.parse(original);
 config.backends[0].location.registry+='/missing';fs.writeFileSync(process.argv[3],JSON.stringify(config));
 await page.getByRole('button',{name:'Apply resource operation'}).click();
 await page.getByRole('alert').filter({hasText:'connection changed'}).waitFor();
 fs.writeFileSync(process.argv[3],original);
 // Lose one reply AFTER the backend committed; reload retains the operation ID.
 await page.route('**/api/resources/actions/*/apply',async route=>{await route.fetch();await route.abort();},{times:1});
 await page.getByRole('button',{name:'Apply resource operation'}).click();
 await page.getByRole('alert').waitFor();await page.reload();
 await page.getByRole('button',{name:'Operation completed'}).waitFor();
 await page.getByRole('button',{name:'Cancel review'}).click();
 await page.getByRole('button',{name:'Refresh resources'}).click();
 await page.getByText('Unpublished; data retained').waitFor();
 await page.getByRole('button',{name:'Review custody transfer…'}).click();
 await page.getByText('New owner: Operator').waitFor();
 await page.getByRole('button',{name:'Apply resource operation'}).click();
 await page.getByText('Resource operation completed.').waitFor();
 const inventory=await (await page.request.get(process.argv[2]+'/api/resources')).json();
 if(inventory.backends[0].inventory.resources[0].owner!=='operator')throw new Error('custody was not transferred');
 fs.writeFileSync(process.argv[3],JSON.stringify(config));await page.getByRole('button',{name:'Refresh resources'}).click();
 await page.getByRole('alert').filter({hasText:'Inventory unavailable'}).waitFor();
 console.log('PASS: browser inventory without cell; review/apply; config drift; lost reply/reload; unpublish; custody transfer; unavailable backend');
}finally{await browser.close();}})().catch(e=>{console.error(e);process.exit(1);});
'''
  print(run(['node','-e',script,playwright,f'http://127.0.0.1:{ui}',config]))
finally:
 for p in reversed(processes):
  p.terminate()
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:p.kill();p.wait()
