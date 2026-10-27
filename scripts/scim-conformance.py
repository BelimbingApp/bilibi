#!/usr/bin/env python3
"""Drive real company SCIM endpoints; no Bilimbi equivalent or unit-test runner.

Use only a disposable walkthrough database. Credentials stay in private scratch
files or environment variables and are never printed. See core/user/scim.md.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid

USER = 'urn:ietf:params:scim:schemas:core:2.0:User'
GROUP = 'urn:ietf:params:scim:schemas:core:2.0:Group'
PATCH = 'urn:ietf:params:scim:api:messages:2.0:PatchOp'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--session-helper', required=True, help='Built scim-walkthrough example; admits a real disposable account session.')
    parser.add_argument('--output', required=True, help='Public resource IDs and results only.')
    args = parser.parse_args()
    base = os.environ['SCIM_BASE_URL'].rstrip('/')
    other = os.environ['SCIM_OTHER_BASE_URL'].rstrip('/')
    credential = Path(os.environ['SCIM_TOKEN_FILE']).read_text().strip()
    origin = os.environ['APP_ORIGIN'].rstrip('/')
    if not base.startswith(origin + '/scim/v2/') or not other.startswith(origin + '/scim/v2/'):
        raise SystemExit('SCIM addresses must use the exact APP_ORIGIN.')
    results = []

    def require(condition, label):
        if not condition:
            raise SystemExit('FAIL: ' + label)
        results.append(label)
        print('PASS: ' + label)

    def call(method, path, document=None, expected=200, endpoint=base, token=credential):
        data = None if document is None else json.dumps(document).encode()
        request = urllib.request.Request(endpoint + path, data=data, method=method,
            headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/scim+json'})
        try:
            response = urllib.request.urlopen(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        status = response.code
        body = response.read()
        if status != expected:
            # Bodies can contain personal values; only safe protocol errors are needed.
            raise SystemExit(f'FAIL: {method} {path.split("?")[0]} returned {status}, expected {expected}')
        if 'application/scim+json' not in response.headers.get('Content-Type', ''):
            raise SystemExit('FAIL: missing SCIM content type')
        return json.loads(body) if body else None

    def patch(path, operations, expected=200):
        return call('PATCH', path, {'schemas': [PATCH], 'Operations': operations}, expected)

    def query(resource, expression, **params):
        return call('GET', '/' + resource + '?' + urllib.parse.urlencode({'filter': expression, **params}))

    require(call('GET', '/ServiceProviderConfig')['patch']['supported'], 'discovery advertises PATCH')
    require(call('GET', '/Schemas')['totalResults'] == 2, 'schema discovery')
    require(call('GET', '/ResourceTypes/User')['endpoint'] == '/Users', 'individual resource-type discovery')
    call('GET', '/Users', expected=401, endpoint=other)
    call('GET', '/Users', expected=401, token='0' * 64)
    require(True, 'foreign company and invalid credentials refused')
    def inspect(email):
        result = subprocess.run([args.session_helper, 'inspect', email], check=True, capture_output=True, text=True)
        return json.loads(result.stdout)

    suffix = uuid.uuid4().hex[:12]
    email = 'scim-' + suffix + '@example.test'
    doc = {'schemas': [USER], 'externalId': 'subject-' + suffix, 'userName': email,
        'displayName': 'Provisioned Person', 'active': True,
        'emails': [{'value': email, 'primary': True, 'type': 'work'}]}
    created = call('POST', '/Users', doc, 201)
    uid = created['id']
    require(created['meta']['location'] == base + '/Users/' + uid, 'create Person, affiliation and account with stable resource URL')
    call('POST', '/Users', doc, expected=409)
    require(query('Users', f'userName eq "{email.upper()}"')['Resources'][0]['id'] == uid, 'case-insensitive uniqueness lookup')
    require(query('Users', f'emails[type eq "work"].value eq "{email}" and active eq true')['totalResults'] == 1, 'email and active conjunction')
    page = call('GET', '/Users?startIndex=1&count=0')
    require(page['itemsPerPage'] == 0 and page['totalResults'] >= 1, 'count-only pagination')
    updated = patch('/Users/' + uid, [{'op': 'Replace', 'path': 'displayName', 'value': 'Updated Person'}, {'op': 'add', 'value': {'name': {'givenName': 'Updated', 'familyName': 'Person'}}}])
    contact = 'contact-' + suffix + '@example.test'
    patch('/Users/' + uid, [{'op': 'replace', 'path': 'emails[type eq "work"].value', 'value': contact}])
    observed = inspect(email)
    require(observed['person_email'] == observed['work_email'] == contact, 'work and Person mailbox updates through their owners')
    require(updated['displayName'] == observed['account_name'] == observed['person_name'] == 'Updated Person', 'profile PATCH updates account and Person')
    updated['displayName'] = 'Replaced Person'
    external_id = updated.pop('externalId')
    replaced = call('PUT', '/Users/' + uid, updated)
    require(replaced['displayName'] == 'Replaced Person' and replaced['externalId'] == external_id, 'User PUT retains an omitted immutable identifier')
    patch('/Users/' + uid, [{'op': 'replace', 'path': 'externalId', 'value': 'changed'}], 400)
    group = call('POST', '/Groups', {'schemas': [GROUP], 'displayName': 'scim-operators', 'externalId': 'group-' + suffix}, 201)
    gid = group['id']
    group_path = '/Groups/' + gid
    added = patch(group_path, [{'op': 'Add', 'path': 'members', 'value': [{'value': uid}]}])
    require(added['members'] == [{'value': uid}] and 'staff' in inspect(email)['roles'], 'group membership immediately grants mapped role')
    require(query('Groups', 'displayName eq "scim-operators"', excludedAttributes='members')['Resources'][0].get('members') is None, 'group lookup excluding members')
    removed = patch(group_path, [{'op': 'Remove', 'path': f'members[value eq "{uid}"]'}])
    require(removed['members'] == [] and 'staff' not in inspect(email)['roles'], 'filtered removal immediately removes mapped role')
    patch(group_path, [{'op': 'add', 'path': 'members', 'value': [{'value': uid}]}])
    require(patch(group_path, [{'op': 'Remove', 'path': 'members', 'value': [{'value': uid}]}])['members'] == [], 'Entra value-list membership removal')
    group['members'] = [{'value': uid}]
    require(call('PUT', group_path, group)['members'] == [{'value': uid}], 'Group PUT restores membership')
    patch(group_path, [{'op': 'add', 'path': 'members', 'value': [{'value': 'foreign-resource'}]}], 400)
    require(True, 'foreign resource membership refused atomically')
    with tempfile.TemporaryDirectory(prefix='bilibi-scim-session-') as temp:
        session_file = Path(temp) / 'session.json'
        subprocess.run([args.session_helper, 'session', email, str(session_file)], check=True, stdout=subprocess.DEVNULL)
        session = json.loads(session_file.read_text())
        opener = urllib.request.build_opener(NoRedirect())
        def session_status():
            request = urllib.request.Request(origin + '/settings/profile', headers={'Cookie': session['cookie']})
            try:
                return opener.open(request, timeout=30).status
            except urllib.error.HTTPError as error:
                return error.code
        require(session_status() == 200, 'real account session admitted before deactivation')
        inactive = patch('/Users/' + uid, [{'op': 'Replace', 'path': 'active', 'value': 'False'}])
        require(inactive['active'] is False and session_status() in (302, 303), 'deactivation ends the admitted session immediately')
        require(query('Users', f'userName eq "{email}"')['Resources'][0]['active'] is False, 'inactive User remains queryable')
        patch(group_path, [{'op': 'remove', 'path': f'members[value eq "{uid}"]'}])
        patch(group_path, [{'op': 'add', 'path': 'members', 'value': [{'value': uid}]}])
        require(inspect(email)['roles'] == [], 'inactive membership changes succeed without restoring grants')
        active = patch('/Users/' + uid, [{'op': 'replace', 'value': {'active': True}}])
        require(active['id'] == uid and active['active'] and session_status() in (302, 303), 'reactivation keeps identifiers and never restores an old session')
    call('DELETE', group_path, expected=204)
    call('GET', group_path, expected=404)
    require(call('GET', '/Users/' + uid)['groups'] == [], 'group deletion removes membership')
    call('DELETE', '/Users/' + uid, expected=204)
    require(call('GET', '/Users/' + uid)['active'] is False, 'DELETE User deactivates without erasure')
    call('GET', '/Users?filter=' + urllib.parse.quote('userName co "example"'), expected=400)
    require(True, 'unsupported filter refused explicitly')
    Path(args.output).write_text(json.dumps({'origin': origin, 'user_id': uid, 'userName': email, 'group_id': gid, 'passed': results}, indent=2) + '\n')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


if __name__ == '__main__':
    main()
