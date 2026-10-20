#!/usr/bin/env python3
"""Disposable-lab proof through chrome-devtools-axi and the public Agent API.
Secrets stay in DATA/private; only redacted answers enter proof/answers.
No Bilimbi counterpart.
"""
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import ssl
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

DATA = Path(os.environ['BILIBI_PROOF_DATA']).resolve()
PRIVATE = DATA / 'private'
ORIGIN = os.environ.get('LAB_ORIGIN', 'https://prod.10.77.61.2.sslip.io')
LABEL = urllib.parse.urlparse(ORIGIN).hostname.split('.')[0]
STATE = PRIVATE / f'{LABEL}.json'
OUT = DATA / 'proof/answers'
ENV = dict(os.environ, CHROME_DEVTOOLS_AXI_SESSION=os.environ.get('CHROME_DEVTOOLS_AXI_SESSION', 'bilibi-fresh-server-proof'),
           CHROME_DEVTOOLS_AXI_USER_DATA_DIR=str(DATA / 'chrome'),
           CHROME_DEVTOOLS_AXI_CHROME_ARGS='--ignore-certificate-errors')
CTX = ssl._create_unverified_context()  # Only the disposable lab's internal CA.
EMAIL = 'owner@proof.invalid'
PASSWORD = (PRIVATE / 'password').read_text().strip()

def state():
    return json.loads(STATE.read_text()) if STATE.exists() else {}

def save(value):
    STATE.write_text(json.dumps(value))
    STATE.chmod(0o600)

def browser(*args):
    return subprocess.run(['chrome-devtools-axi', *args], env=ENV, text=True,
                          capture_output=True, check=True).stdout

def browser_script(script):
    return subprocess.run(['chrome-devtools-axi', 'run'], input=script,
                          env=ENV, text=True, capture_output=True, check=True).stdout.strip()

def js(body, asynchronous=False):
    out = browser('eval', ('async ' if asynchronous else '') + '() => {' + body + '}')
    line = next(line for line in out.splitlines() if line.startswith('result: '))
    value = json.loads(line.removeprefix('result: '))
    try:
        return json.loads(value)
    except (ValueError, TypeError):
        return value

def fill(selector, value):
    js(f'const e=document.querySelector({json.dumps(selector)}); if(!e) throw Error("Missing field"); e.value={json.dumps(value)}; e.dispatchEvent(new Event("input",{{bubbles:true}})); e.dispatchEvent(new Event("change",{{bubbles:true}})); return true;')

def click(selector):
    js(f'const e=document.querySelector({json.dumps(selector)}); if(!e) throw Error("Missing control"); e.click(); return true;')
    time.sleep(0.6)

def redact(value):
    if isinstance(value, dict):
        return {k: ('[redacted]' if k in ('plan_code', 'access_token', 'refresh_token', 'device_code', 'user_code') else redact(v)) for k,v in value.items()}
    if isinstance(value, list):
        return [redact(v) for v in value]
    return value

def request(method, path, body=None, api=True):
    headers = {'Accept': 'application/json'}
    if api:
        headers.update({'Authorization': 'Bearer ' + state()['bearer'],
                        'x-bilibi-agent-id': 'fresh-server-proof',
                        'X-Bilibi-Agent-Self-Report': 'codex/gpt-6-high'})
    if body is not None:
        if api:
            body = json.dumps(body).encode()
            headers['Content-Type'] = 'application/json'
        else:
            body = urllib.parse.urlencode(body).encode()
            headers['Content-Type'] = 'application/x-www-form-urlencoded'
    req = urllib.request.Request(ORIGIN + path, data=body, headers=headers, method=method)
    try:
        r = urllib.request.urlopen(req, context=CTX, timeout=70)
    except urllib.error.HTTPError as exc:
        r = exc
    if r.headers.get('X-Bilibi-Waiting'):
        return r.status, {'waiting_page': True}
    raw = r.read().decode()
    try:
        return r.status, json.loads(raw)
    except ValueError:
        return r.status, {'non_json': raw[:150]}

def answer(name, value):
    (OUT / f'{LABEL}-{name}.json').write_text(json.dumps(redact(value), indent=2) + '\n')

def code():
    st = state()
    while int(time.time() // 30) == st.get('used_step'):
        time.sleep(1)
    step = int(time.time() // 30)
    key = st['key']
    key = base64.b32decode(key + '=' * ((8-len(key)%8)%8))
    digest = hmac.new(key, struct.pack('>Q', step), hashlib.sha1).digest()
    offset = digest[-1] & 15
    result = str((int.from_bytes(digest[offset:offset+4], 'big') & 0x7fffffff) % 1000000).zfill(6)
    st['used_step'] = step
    save(st)
    return result

def begin():
    status, value = request('POST', '/oauth/device_authorization', {'client_id': 'bilibi-native', 'agent_id': 'fresh-server-proof'}, api=False)
    assert status == 200, status
    st = state()
    st['device'] = value
    save(st)
    print('Device request saved privately.')

def token():
    status, value = request('POST', '/oauth/token', {'grant_type': 'urn:ietf:params:oauth:grant-type:device_code', 'device_code': state()['device']['device_code'], 'client_id': 'bilibi-native'}, api=False)
    assert status == 200 and 'access_token' in value, status
    st = state()
    st['bearer'] = value['access_token']
    del st['device']
    save(st)
    print('Bearer issued through browser-approved device flow; stored privately.')

def plan_commit(op, no_code=False, refuse_first=False, refuse_only=False):
    status, plan = request('POST', '/api/v1/plans', {'operations': [op]})
    answer('plan-' + op['key'], {'status': status, 'body': plan})
    print('Plan:', status, 'valid=', plan.get('valid'))
    print(json.dumps(redact(plan.get('results')), indent=1))
    assert status == 200 and plan.get('valid') and plan.get('plan_code'), 'Invalid plan'
    body = {'plan_code': plan['plan_code'], 'operations': [op]}
    if refuse_first:
        status, refused = request('POST', '/api/v1/plans/commit', body)
        answer('no-code-' + op['key'], {'status': status, 'body': refused})
        assert status == 403 and refused.get('error') == 'step_up_required', redact(refused)
        print('Commit without a code refused:', refused.get('error'), refused.get('reason'))
        if refuse_only:
            return refused
    if not no_code:
        body['step_up_code'] = code()
    status, committed = request('POST', '/api/v1/plans/commit', body)
    answer('commit-' + op['key'], {'status': status, 'body': committed})
    print('Commit:', status, json.dumps(redact(committed)))
    assert status == 200, 'Commit failed'
    return committed

def github_input(asset=None):
    # The saved list first: unauthenticated GitHub allows 60 calls an hour.
    status, value = request('POST', '/api/v1/operations/base.software.releases/call', {'input': {'prerelease': True}})
    github = value['result']['github']
    if not github.get('releases'):
        status, value = request('POST', '/api/v1/operations/base.software.releases/call', {'input': {'refresh': True, 'prerelease': True}})
        github = value['result']['github']
    rows = github['releases']
    row = next(r for r in rows if asset is None or str(r['asset_id']) == str(asset))
    return {'asset': row['asset_id'], 'repository_id': github['repository_id'], 'release_id': row['release_id'],
            'tag': row['tag'], 'target': row['target'], 'digest': row['digest'], 'switch': 'auto'}

cmd, *args = sys.argv[1:]
if cmd == 'login':
    browser('open', ORIGIN + '/login')
    fill('input[type=email]', EMAIL)
    fill('input[type=password]', PASSWORD)
    click('button[type=submit]')
    print(js('return {path:location.pathname,signedIn:!!document.querySelector("#app-shell")};'))
elif cmd == 'enrol':
    browser('open', ORIGIN + '/settings/two-factor')
    fill('input[type=password]', PASSWORD)
    click('#two-factor-start')
    key = js('return document.querySelector("#two-factor-key").textContent.replaceAll(" ","");')
    st = state()
    st['key'] = key
    save(st)
    fill('input[name="two_factor[code]"]', code())
    click('#two-factor-confirm')
    assert js('return !!document.querySelector("#two-factor-on");')
    print('2FA enrolled in the browser; key omitted from evidence.')
elif cmd == 'authorize':
    begin()
    browser('open', ORIGIN + '/oauth/device')
    fill('input[name=user_code]', state()['device']['user_code'])
    click('#device-lookup')
    click('#device-approve')
    assert js('return !!document.querySelector("#device-completed");')
    token()
elif cmd == 'read':
    status, value = request('POST', '/api/v1/operations/' + args[0] + '/call', {'input': {}})
    answer(args[0], {'status': status, 'body': value})
    print(status, json.dumps(redact(value), indent=1))
elif cmd == 'plan':
    # Any lab operation from task material, including a retained Staging report.
    op = json.loads(Path(args[0]).read_text())
    plan_commit(op, no_code=('no-code' in args[1:]), refuse_first=('refuse-first' in args[1:]))
elif cmd == 'plan-only':
    op = json.loads(Path(args[0]).read_text())
    status, value = request('POST', '/api/v1/plans', {'operations': [op]})
    answer(Path(args[0]).stem, {'status': status, 'body': value})
    assert status == 200, status
    assert value.get('valid') == ('refused' not in args[1:]), redact(value)
    print(json.dumps(redact(value.get('results')), indent=1))
elif cmd == 'install':
    plan_commit({'key': 'base.software.install', 'input': {'bundle': args[0], 'switch': 'auto'}}, refuse_first=True)
elif cmd == 'refuse-install':
    plan_commit({'key': 'base.software.install', 'input': {'bundle': args[0], 'switch': 'auto'}}, refuse_first=True, refuse_only=True)
elif cmd == 'github-input':
    # The GitHub row the page would review, built from base.software.releases.
    print(json.dumps(github_input(args[0] if args else None)))
elif cmd == 'github-install':
    # github-install [asset-id] [refuse|wrong-digest|wrong-repository]
    asset = args[0] if args and args[0].isdigit() else None
    mode = next((a for a in args if not a.isdigit()), '')
    value = github_input(asset)
    if mode == 'wrong-digest':
        value['digest'] = '0' * 64
    elif mode == 'wrong-repository':
        value['repository_id'] += 1
    plan_commit({'key': 'base.software.install', 'input': value}, refuse_first=True, refuse_only=(mode == 'refuse'))
elif cmd == 'cancel':
    plan_commit({'key': 'base.software.cancel', 'input': {}}, no_code=True)
elif cmd == 'confirm-page':
    # The host's confirm page, after a page action asked for a fresh code.
    fill('input[name*=code]', code())
    js('const form=document.querySelector("input[name*=code]").closest("form"); form.querySelector("button[type=submit]").click(); return true;')
    time.sleep(1.5)
    print(js('return location.pathname + location.search;'))
elif cmd == 'review':
    browser('open', ORIGIN + '/system/software?tab=releases')
    fill('#software-bundle', args[0])
    click('#software-review-install')
    assert js('return document.querySelector("#software-review").open;')
    print(browser('snapshot'))
elif cmd == 'confirm':
    fill('#software-code', code())
    click('#software-review-confirm')
    print('Submitted the reviewed install with a fresh authenticator code.')
elif cmd == 'receive':
    click('#release-files-upload-submit')
    deadline = time.time() + 60
    while time.time() < deadline:
        if js('return document.body.textContent.includes("Received release");'):
            break
        time.sleep(0.5)
    else:
        raise SystemExit('The release file was not received within 60 seconds')
    print(browser('snapshot'))
elif cmd == 'undo':
    plan_commit({'key': 'base.software.undo', 'input': {}}, refuse_first=True)
elif cmd == 'wait':
    job_id = int(args[0])
    deadline = time.time() + 180
    seen = set()
    while time.time() < deadline:
        status, value = request('POST', '/api/v1/operations/base.software.status/call', {'input': {}})
        job = (value.get('result') or {}).get('job') or {}
        if value.get('waiting_page') and 'waiting' not in seen:
            seen.add('waiting')
            print('Observed 503 waiting page while the installer paused the app.', flush=True)
        for step in job.get('steps', []):
            transition = (step['key'], step['state'])
            if transition not in seen:
                seen.add(transition)
                print('Step:', step['key'], step['state'], (step.get('detail') or {}).get('message'), flush=True)
        if job.get('id') == job_id and job.get('state') not in ('running', 'queued'):
            answer('job-' + args[0], {'status': status, 'body': value})
            print(json.dumps(job, indent=1))
            break
        time.sleep(1)
    else:
        raise SystemExit('Job did not finish within 180 seconds')
elif cmd == 'upload':
    browser('open', ORIGIN + '/system/software?tab=releases')
    # AXI's native upload currently sends filePath; newer DevTools expects
    # filePaths. Populate the real input with a File through AXI instead.
    js('const fold=document.getElementById("software-offline-upload");if(fold) fold.open=true;return true;')
    target = Path(args[0]).resolve() if args else DATA / 'proof/release.tar.gz'
    js('window.proofUploadParts=[]; return true;')
    with target.open('rb') as source:
        while chunk := source.read(256 * 1024):
            encoded = base64.b64encode(chunk).decode()
            script = 'await page.eval(() => {const s=atob(' + json.dumps(encoded) + ');window.proofUploadParts.push(Uint8Array.from(s,c=>c.charCodeAt(0)));});'
            browser_script(script)
    size = js('const f=new File(window.proofUploadParts,"bilibi-setup-upload.tar.gz",{type:"application/gzip"});const t=new DataTransfer();t.items.add(f);const e=document.querySelector("input[type=file]");e.files=t.files;e.dispatchEvent(new Event("change",{bubbles:true}));delete window.proofUploadParts;return f.size;')
    assert size == target.stat().st_size
    print('Selected the release archive in the browser:', size, 'bytes.')
elif cmd == 'snapshot':
    print(browser('snapshot'))
elif cmd == 'shot':
    print(browser('screenshot', str(DATA / 'proof/shots' / args[0])))
else:
    raise SystemExit('Unknown command')
