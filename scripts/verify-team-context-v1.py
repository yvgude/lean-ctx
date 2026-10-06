#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Conformance verifier for the team-context-v1 contract fixtures.

WHAT THIS PROGRAM VERIFIES
--------------------------
It verifies the *fixtures* in docs/contracts/team-context-v1/fixtures/ against
the contract text in docs/contracts/team-context-v1/README.md:

  * RFC 8785 (JCS) canonicalization: it recanonicalizes every vector from its
    input object and requires an exact byte match with the committed canonical
    string, the committed base64 of the canonical UTF-8 bytes, the committed
    byte length and the committed SHA-256.
  * Ed25519 signing preimages: it rebuilds the signing string from the record
    digest computed with `signature_envelope_digest` omitted, and verifies the
    committed signature with a pure-Python Ed25519 implementation.
  * Tenant-keyed pseudonyms and nonces: it recomputes HMAC-SHA-256 and SHA-256
    from the committed test-only preimages.
  * The 12 mandatory cross-field / cross-record rules in the README, evaluated
    over self-contained record bundles. Every digest reference between records
    in a bundle is resolved by recomputing the referenced record's digest.
  * That the case inventory covers exactly rules 1..12.

JSON SCHEMA SCOPE
-----------------
  * It loads the bundled schemas and validates every materialized cross-record
    fixture with the dependency-free Draft 2020-12 subset used by this pack.
  * It is not a general JSON Schema metaschema validator. Run this separately:
        cd docs/contracts/team-context-v1 && \
        check-jsonschema --check-metaschema ./*.schema.json
  * It does NOT prove that any server, PostgreSQL deployment or runtime
    enforces these rules. It proves only that the committed fixtures are
    internally consistent and that the rules are decidable from the record
    graph alone. Runtime acceptance is a separate, unmet gate.
  * It does NOT check tenant isolation, authentication, replay windows, clock
    skew against a real clock, or anything requiring a database or a network.

Dependency-free: Python 3.9+ standard library only. Deterministic: stdout for
an unchanged fixture set is byte-identical across runs.

Usage:  python3 scripts/verify-team-context-v1.py [--fixtures DIR]
Exit:   0 = all checks passed, 1 = at least one check failed, 2 = usage error.
"""

import argparse
import base64
import binascii
import copy
import hashlib
import hmac
import json
import os
import re
import sys
import unicodedata

# --------------------------------------------------------------------------
# RFC 8785 (JCS) canonicalization
# --------------------------------------------------------------------------

_ESC = {'"': '\\"', '\\': '\\\\', '\b': '\\b', '\f': '\\f',
        '\n': '\\n', '\r': '\\r', '\t': '\\t'}

MAX_SAFE_INTEGER = 9007199254740991


class JcsError(ValueError):
    pass


def _jcs_string(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch in _ESC:
            out.append(_ESC[ch])
        elif o < 0x20:
            out.append('\\u%04x' % o)
        elif 0xD800 <= o <= 0xDFFF:
            raise JcsError('lone surrogate U+%04X is not valid JSON text' % o)
        else:
            out.append(ch)
    out.append('"')
    return ''.join(out)


def _utf16_sort_key(s):
    # Comparing UTF-16BE byte strings is equivalent to comparing UTF-16
    # code-unit sequences numerically, which is what RFC 8785 requires.
    return s.encode('utf-16-be', 'surrogatepass')


def _jcs(v):
    if v is None:
        return 'null'
    if v is True:
        return 'true'
    if v is False:
        return 'false'
    if isinstance(v, int):
        if not (-MAX_SAFE_INTEGER <= v <= MAX_SAFE_INTEGER):
            raise JcsError('integer outside exactly-representable range')
        return str(v)
    if isinstance(v, float):
        # team-context-v1 defines no non-integer numeric field. RFC 8785
        # number serialization for non-integers follows ECMAScript
        # Number::toString; rather than approximate it, fail closed.
        raise JcsError('non-integer number is out of scope for team-context-v1')
    if isinstance(v, str):
        return _jcs_string(v)
    if isinstance(v, list):
        return '[' + ','.join(_jcs(x) for x in v) + ']'
    if isinstance(v, dict):
        parts = []
        for k in sorted(v.keys(), key=_utf16_sort_key):
            if not isinstance(k, str):
                raise JcsError('non-string object member name')
            parts.append(_jcs_string(k) + ':' + _jcs(v[k]))
        return '{' + ','.join(parts) + '}'
    raise JcsError('unserializable type %s' % type(v).__name__)


def canonical_string(v):
    return _jcs(v)


def canonical_bytes(v):
    return _jcs(v).encode('utf-8')


def record_digest(record):
    return 'sha256:' + hashlib.sha256(canonical_bytes(record)).hexdigest()


def preimage_digest(record):
    """Record digest with the record's own signature_envelope_digest omitted."""
    return record_digest({k: v for k, v in record.items()
                          if k != 'signature_envelope_digest'})


def provenance_subject_digest(record):
    return record_digest({k: v for k, v in record.items()
                          if k != 'provenance_digest'})


# --------------------------------------------------------------------------
# Ed25519 verification (RFC 8032), pure Python, no dependencies
# --------------------------------------------------------------------------

_P = 2 ** 255 - 19
_L = 2 ** 252 + 27742317777372353535851937790883648493


def _inv(x):
    return pow(x, _P - 2, _P)


_D = (-121665 * _inv(121666)) % _P
_I = pow(2, (_P - 1) // 4, _P)


def _xrecover(y):
    xx = (y * y - 1) * _inv(_D * y * y + 1)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P != 0:
        x = (x * _I) % _P
    if x % 2 != 0:
        x = _P - x
    return x


_BY = (4 * _inv(5)) % _P
_BX = _xrecover(_BY)
_B = (_BX % _P, _BY % _P)


def _edwards_add(p, q):
    x1, y1 = p
    x2, y2 = q
    t = _D * x1 * x2 * y1 * y2
    x3 = (x1 * y2 + x2 * y1) * _inv(1 + t)
    y3 = (y1 * y2 + x1 * x2) * _inv(1 - t)
    return (x3 % _P, y3 % _P)


def _scalarmult(p, e):
    q = (0, 1)
    # Iterative double-and-add; avoids deep recursion.
    for bit in bin(e)[2:]:
        q = _edwards_add(q, q)
        if bit == '1':
            q = _edwards_add(q, p)
    return q


def _is_on_curve(p):
    x, y = p
    return (-x * x + y * y - 1 - _D * x * x * y * y) % _P == 0


def _decode_point(s):
    y = int.from_bytes(s, 'little') & ((1 << 255) - 1)
    if y >= _P:
        return None
    x = _xrecover(y)
    if x == 0 and ((s[31] >> 7) & 1):
        return None
    if (x & 1) != ((s[31] >> 7) & 1):
        x = _P - x
    p = (x % _P, y % _P)
    return p if _is_on_curve(p) else None


_IDENTITY = (0, 1)


def _is_strict_prime_order_point(point):
    return (point is not None
            and _scalarmult(point, 8) != _IDENTITY
            and _scalarmult(point, _L) == _IDENTITY)


_ED_CACHE = {}


def ed25519_verify(public_key, message, signature):
    key = (public_key, message, signature)
    if key in _ED_CACHE:
        return _ED_CACHE[key]
    result = False
    if len(public_key) == 32 and len(signature) == 64:
        a = _decode_point(public_key)
        r = _decode_point(signature[:32])
        s = int.from_bytes(signature[32:], 'little')
        if (_is_strict_prime_order_point(a)
                and _is_strict_prime_order_point(r) and s < _L):
            h = int.from_bytes(
                hashlib.sha512(signature[:32] + public_key + message).digest(),
                'little') % _L
            result = _edwards_add(r, _scalarmult(a, h)) == _scalarmult(_B, s)
    _ED_CACHE[key] = result
    return result


def _ed25519_selftest():
    """RFC 8032 section 7.1 TEST 1 and TEST 2, plus a negative."""
    pub1 = binascii.unhexlify(
        'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a')
    sig1 = binascii.unhexlify(
        'e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555f'
        'b8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b')
    pub2 = binascii.unhexlify(
        '3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c')
    sig2 = binascii.unhexlify(
        '92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da08'
        '5ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00')
    identity = b'\x01' + b'\x00' * 31
    forgery = identity + b'\x00' * 32
    ok = (ed25519_verify(pub1, b'', sig1)
          and ed25519_verify(pub2, b'\x72', sig2)
          and not ed25519_verify(pub1, b'x', sig1)
          and not ed25519_verify(identity, b'arbitrary', forgery))
    return ok


def b64u_decode(s, expected_len, what):
    if not isinstance(s, str):
        raise ValueError('%s is not a string' % what)
    if not re.fullmatch(r'[A-Za-z0-9_-]+', s):
        raise ValueError('%s is not canonical unpadded base64url' % what)
    pad = '=' * (-len(s) % 4)
    try:
        raw = base64.urlsafe_b64decode(s + pad)
    except (binascii.Error, ValueError):
        raise ValueError('%s is not valid unpadded base64url' % what)
    if len(raw) != expected_len:
        raise ValueError('%s decodes to %d bytes, expected %d'
                         % (what, len(raw), expected_len))
    if base64.urlsafe_b64encode(raw).decode('ascii').rstrip('=') != s:
        raise ValueError('%s is not canonical unpadded base64url' % what)
    return raw


# --------------------------------------------------------------------------
# Fail-closed fixture shape checking
# --------------------------------------------------------------------------

class FixtureError(Exception):
    pass


def require_object(value, where):
    if not isinstance(value, dict):
        raise FixtureError('%s: expected an object' % where)
    return value


def check_members(obj, where, required, optional=()):
    """Fail closed: every required member present, no unknown member."""
    if not isinstance(obj, dict):
        raise FixtureError('%s: expected an object' % where)
    keys = set(obj)
    missing = sorted(set(required) - keys)
    if missing:
        raise FixtureError('%s: missing member(s): %s'
                           % (where, ', '.join(missing)))
    unknown = sorted(keys - set(required) - set(optional))
    if unknown:
        raise FixtureError('%s: unknown member(s): %s'
                           % (where, ', '.join(unknown)))
    return obj


def require_str(obj, key, where):
    v = obj.get(key)
    if not isinstance(v, str):
        raise FixtureError('%s.%s: expected a string' % (where, key))
    return v


def require_int(obj, key, where):
    v = obj.get(key)
    if not isinstance(v, int) or isinstance(v, bool):
        raise FixtureError('%s.%s: expected an integer' % (where, key))
    return v


def require_bool_true(obj, key, where):
    if obj.get(key) is not True:
        raise FixtureError('%s.%s: expected true' % (where, key))


def load_json(path):
    try:
        with open(path, 'rb') as fh:
            raw = fh.read()
    except OSError as exc:
        raise FixtureError('cannot read %s: %s' % (os.path.basename(path), exc))
    try:
        def unique_object(pairs):
            out = {}
            for key, value in pairs:
                if key in out:
                    raise FixtureError('%s: duplicate object member %r'
                                       % (os.path.basename(path), key))
                out[key] = value
            return out
        return json.loads(raw.decode('utf-8'), object_pairs_hook=unique_object)
    except (UnicodeDecodeError, ValueError) as exc:
        raise FixtureError('%s is not valid UTF-8 JSON: %s'
                           % (os.path.basename(path), exc))


def load_contract_schemas(contract_dir):
    schemas = {}
    for name in sorted(os.listdir(contract_dir)):
        if name.endswith('.schema.json'):
            schemas[name] = load_json(os.path.join(contract_dir, name))
    return schemas


def verify_frozen_hashes(contract_dir, rep):
    manifest_path = os.path.join(contract_dir, 'frozen-hashes.json')
    manifest = load_json(manifest_path)
    check_members(manifest, 'frozen-hashes.json',
                  required=('manifest_version', 'algorithm', 'excludes', 'files'))
    if manifest['manifest_version'] != 1 or manifest['algorithm'] != 'sha256':
        raise FixtureError('frozen-hashes.json: unsupported manifest contract')
    if manifest['excludes'] != ['frozen-hashes.json']:
        raise FixtureError('frozen-hashes.json: self-exclusion must be explicit and exclusive')
    files = require_object(manifest['files'], 'frozen-hashes.json.files')
    expected = {'README.md'}
    expected.update(name for name in os.listdir(contract_dir)
                    if name.endswith('.schema.json'))
    fixtures_dir = os.path.join(contract_dir, 'fixtures')
    expected.update('fixtures/' + name for name in os.listdir(fixtures_dir)
                    if name.endswith('.json'))
    expected.add('../../../scripts/verify-team-context-v1.py')
    if set(files) != expected:
        raise FixtureError('frozen-hashes.json: file inventory differs: missing=%s extra=%s'
                           % (sorted(expected - set(files)), sorted(set(files) - expected)))
    for relative in sorted(files):
        path = os.path.normpath(os.path.join(contract_dir, relative))
        with open(path, 'rb') as handle:
            actual = hashlib.sha256(handle.read()).hexdigest()
        expected_hash = files[relative]
        if not re.fullmatch(r'[0-9a-f]{64}', expected_hash):
            raise FixtureError('frozen-hashes.json: invalid hash for %s' % relative)
        if actual != expected_hash:
            rep.fail('freeze/%s' % relative,
                     'sha256 mismatch: computed %s' % actual)
        else:
            rep.ok('freeze/%s' % relative)
    rep.count('frozen_files', len(files))


def _schema_target(ref, schemas, current_name):
    file_name, _, fragment = ref.partition('#')
    name = file_name or current_name
    if name not in schemas:
        raise FixtureError('schema reference targets unknown file %r' % name)
    target = schemas[name]
    if fragment:
        if not fragment.startswith('/'):
            raise FixtureError('unsupported schema fragment %r' % fragment)
        for token in fragment[1:].split('/'):
            token = token.replace('~1', '/').replace('~0', '~')
            if not isinstance(target, dict) or token not in target:
                raise FixtureError('dangling schema reference %r' % ref)
            target = target[token]
    return name, target


def schema_errors(value, schema, schemas, current_name, path='$'):
    """Evaluate the asserted Draft 2020-12 subset used by this contract pack."""
    errors = []
    if '$ref' in schema:
        ref_name, target = _schema_target(schema['$ref'], schemas, current_name)
        errors.extend(schema_errors(value, target, schemas, ref_name, path))
    if 'const' in schema and value != schema['const']:
        errors.append('%s must equal %r' % (path, schema['const']))
    if 'enum' in schema and value not in schema['enum']:
        errors.append('%s is outside enum' % path)
    type_name = schema.get('type')
    type_ok = {
        'object': isinstance(value, dict),
        'array': isinstance(value, list),
        'string': isinstance(value, str),
        'integer': isinstance(value, int) and not isinstance(value, bool),
        'boolean': isinstance(value, bool),
        'null': value is None,
    }.get(type_name, True)
    if type_name is not None and not type_ok:
        errors.append('%s must be %s' % (path, type_name))
        return errors
    if isinstance(value, dict):
        required = schema.get('required', [])
        errors.extend('%s.%s is required' % (path, key)
                      for key in required if key not in value)
        properties = schema.get('properties', {})
        if schema.get('additionalProperties') is False:
            errors.extend('%s.%s is unknown' % (path, key)
                          for key in value if key not in properties)
        for key, child in properties.items():
            if key in value:
                errors.extend(schema_errors(value[key], child, schemas,
                                            current_name, path + '.' + key))
    if isinstance(value, list):
        if len(value) < schema.get('minItems', 0):
            errors.append('%s has too few items' % path)
        if 'maxItems' in schema and len(value) > schema['maxItems']:
            errors.append('%s has too many items' % path)
        if schema.get('uniqueItems'):
            encoded = [canonical_string(item) for item in value]
            if len(set(encoded)) != len(encoded):
                errors.append('%s items are not unique' % path)
        if 'items' in schema:
            for index, item in enumerate(value):
                errors.extend(schema_errors(item, schema['items'], schemas,
                                            current_name, '%s[%d]' % (path, index)))
    if isinstance(value, str):
        if len(value) < schema.get('minLength', 0):
            errors.append('%s is too short' % path)
        if 'maxLength' in schema and len(value) > schema['maxLength']:
            errors.append('%s is too long' % path)
        if 'pattern' in schema and re.search(schema['pattern'], value) is None:
            errors.append('%s does not match pattern' % path)
    if isinstance(value, int) and not isinstance(value, bool):
        if 'minimum' in schema and value < schema['minimum']:
            errors.append('%s is below minimum' % path)
        if 'maximum' in schema and value > schema['maximum']:
            errors.append('%s is above maximum' % path)
    for child in schema.get('allOf', []):
        errors.extend(schema_errors(value, child, schemas, current_name, path))
    if 'anyOf' in schema and not any(
            not schema_errors(value, child, schemas, current_name, path)
            for child in schema['anyOf']):
        errors.append('%s matches no anyOf branch' % path)
    if 'oneOf' in schema:
        matches = sum(not schema_errors(value, child, schemas, current_name, path)
                      for child in schema['oneOf'])
        if matches != 1:
            errors.append('%s matches %d oneOf branches' % (path, matches))
    if 'not' in schema and not schema_errors(
            value, schema['not'], schemas, current_name, path):
        errors.append('%s matches forbidden schema' % path)
    if 'if' in schema:
        branch = 'then' if not schema_errors(
            value, schema['if'], schemas, current_name, path) else 'else'
        if branch in schema:
            errors.extend(schema_errors(value, schema[branch], schemas,
                                        current_name, path))
    return errors


# --------------------------------------------------------------------------
# Result accumulation (deterministic reporting)
# --------------------------------------------------------------------------

class Report(object):
    def __init__(self):
        self.lines = []
        self.failures = []
        self.counts = {}

    def section(self, title):
        self.lines.append('')
        self.lines.append('== %s' % title)

    def ok(self, label, detail=''):
        self.lines.append('  PASS  %-52s %s' % (label, detail))

    def fail(self, label, detail):
        self.lines.append('  FAIL  %-52s %s' % (label, detail))
        self.failures.append('%s: %s' % (label, detail))

    def note(self, text):
        self.lines.append('  note  %s' % text)

    def count(self, key, n):
        self.counts[key] = n


# --------------------------------------------------------------------------
# Canonicalization fixture
# --------------------------------------------------------------------------

VECTOR_REQ = ('id', 'description', 'input', 'canonical_json',
              'canonical_utf8_base64', 'canonical_utf8_byte_length', 'sha256')
VECTOR_OPT = ('omit_members',)

SIGNING_REQ = ('id', 'description', 'signed_payload_kind', 'record',
               'signed_payload_digest', 'signing_string',
               'signing_string_utf8_base64', 'public_key', 'signature',
               'expect_valid')


def verify_canonicalization(path, rep):
    doc = load_json(path)
    check_members(doc, 'canonicalization.json',
                  required=('fixture_kind', 'fixture_version', 'non_secret',
                            'description', 'spec', 'vectors', 'signing_inputs',
                            'negative_inputs'))
    if doc['fixture_kind'] != 'team-context-v1/canonicalization':
        raise FixtureError('canonicalization.json: unexpected fixture_kind %r'
                           % doc['fixture_kind'])
    if doc['fixture_version'] != 1:
        raise FixtureError('canonicalization.json: unsupported fixture_version')
    require_bool_true(doc, 'non_secret', 'canonicalization.json')

    vectors = doc['vectors']
    if not isinstance(vectors, list) or not vectors:
        raise FixtureError('canonicalization.json: vectors must be a non-empty array')

    seen = set()
    for i, vec in enumerate(vectors):
        where = 'canonicalization.json vectors[%d]' % i
        check_members(vec, where, VECTOR_REQ, VECTOR_OPT)
        vid = require_str(vec, 'id', where)
        if vid in seen:
            raise FixtureError('%s: duplicate vector id %r' % (where, vid))
        seen.add(vid)

        value = vec['input']
        if not isinstance(value, dict):
            raise FixtureError('%s: input must be an object' % where)
        omit = vec.get('omit_members', [])
        if not isinstance(omit, list) or any(not isinstance(m, str) for m in omit):
            raise FixtureError('%s: omit_members must be an array of strings' % where)
        for member in omit:
            if member not in value:
                raise FixtureError('%s: omit_members names absent member %r'
                                   % (where, member))
        if omit:
            value = {k: v for k, v in value.items() if k not in omit}

        try:
            got = canonical_string(value)
        except JcsError as exc:
            rep.fail('canonical/%s' % vid, 'canonicalization refused: %s' % exc)
            continue

        want = require_str(vec, 'canonical_json', where)
        if got != want:
            rep.fail('canonical/%s' % vid,
                     'canonical string mismatch: computed %s' % json.dumps(got))
            continue
        got_bytes = got.encode('utf-8')
        want_bytes = b64u_decode(vec['canonical_utf8_base64'], len(got_bytes),
                                 '%s canonical_utf8_base64' % where) \
            if isinstance(vec['canonical_utf8_base64'], str) else None
        if want_bytes != got_bytes:
            rep.fail('canonical/%s' % vid, 'canonical UTF-8 bytes mismatch')
            continue
        if require_int(vec, 'canonical_utf8_byte_length', where) != len(got_bytes):
            rep.fail('canonical/%s' % vid, 'canonical_utf8_byte_length mismatch')
            continue
        digest = 'sha256:' + hashlib.sha256(got_bytes).hexdigest()
        if require_str(vec, 'sha256', where) != digest:
            rep.fail('canonical/%s' % vid,
                     'sha256 mismatch: computed %s' % digest)
            continue
        rep.ok('canonical/%s' % vid, '%d bytes' % len(got_bytes))

    signing = doc['signing_inputs']
    if not isinstance(signing, list) or not signing:
        raise FixtureError('canonicalization.json: signing_inputs must be non-empty')
    for i, item in enumerate(signing):
        where = 'canonicalization.json signing_inputs[%d]' % i
        check_members(item, where, SIGNING_REQ)
        sid = require_str(item, 'id', where)
        record = require_object(item['record'], '%s.record' % where)
        if 'signature_envelope_digest' not in record:
            raise FixtureError('%s.record: expected a signature_envelope_digest '
                               'member so the omission preimage is meaningful' % where)
        kind = require_str(item, 'signed_payload_kind', where)
        computed = preimage_digest(record)
        if require_str(item, 'signed_payload_digest', where) != computed:
            rep.fail('signing/%s' % sid,
                     'omission preimage digest mismatch: computed %s' % computed)
            continue
        expected_string = 'leanctx-team-context-v1:%s:%s' % (kind, computed)
        if require_str(item, 'signing_string', where) != expected_string:
            rep.fail('signing/%s' % sid, 'signing string does not match the '
                                         'contract construction')
            continue
        msg = expected_string.encode('ascii')
        b64 = b64u_decode(item['signing_string_utf8_base64'], len(msg),
                          '%s signing_string_utf8_base64' % where)
        if b64 != msg:
            rep.fail('signing/%s' % sid, 'signing_string_utf8_base64 mismatch')
            continue
        pub = b64u_decode(item['public_key'], 32, '%s public_key' % where)
        sig = b64u_decode(item['signature'], 64, '%s signature' % where)
        expect_valid = item['expect_valid']
        if not isinstance(expect_valid, bool):
            raise FixtureError('%s.expect_valid: expected a boolean' % where)
        valid = ed25519_verify(pub, msg, sig)
        if valid != expect_valid:
            rep.fail('signing/%s' % sid,
                     'Ed25519 verification returned %s, fixture expects %s'
                     % (valid, expect_valid))
            continue
        rep.ok('signing/%s' % sid,
               'ed25519 %s' % ('verifies' if expect_valid else 'correctly rejected'))

    rep.count('canonicalization_vectors', len(vectors))
    rep.count('signing_inputs', len(signing))

    negatives = doc['negative_inputs']
    if not isinstance(negatives, list) or not negatives:
        raise FixtureError('canonicalization.json: negative_inputs must be non-empty')
    for i, item in enumerate(negatives):
        where = 'canonicalization.json negative_inputs[%d]' % i
        check_members(item, where,
                      required=('id', 'kind', 'value', 'description'))
        nid = require_str(item, 'id', where)
        kind = require_str(item, 'kind', where)
        value = require_str(item, 'value', where)
        rejected = False
        try:
            if kind == 'base64url-32':
                b64u_decode(value, 32, where)
            elif kind == 'json':
                def unique_object(pairs):
                    result = {}
                    for key, child in pairs:
                        if key in result:
                            raise ValueError('duplicate object member')
                        result[key] = child
                    return result
                json.loads(value, object_pairs_hook=unique_object)
            else:
                raise FixtureError('%s.kind: unsupported negative kind %r'
                                   % (where, kind))
        except (ValueError, binascii.Error):
            rejected = True
        if rejected:
            rep.ok('parser-negative/%s' % nid, 'rejected fail-closed')
        else:
            rep.fail('parser-negative/%s' % nid, 'malformed input was accepted')
    rep.count('parser_negative_inputs', len(negatives))


# --------------------------------------------------------------------------
# Pseudonym and nonce fixture
# --------------------------------------------------------------------------

PSEUDO_REQ = ('id', 'description', 'subject', 'subject_utf8_base64',
              'lowercased_nfc', 'lowercased_nfc_utf8_base64', 'hmac_sha256')
NONCE_REQ = ('id', 'description', 'nonce_hex', 'nonce_bit_length', 'sha256')

MIN_NONCE_BITS = 128
MIN_PEPPER_BITS = 256


def verify_pseudonyms(path, rep):
    doc = load_json(path)
    check_members(doc, 'pseudonyms.json',
                  required=('fixture_kind', 'fixture_version', 'non_secret',
                            'description', 'pepper', 'pseudonym_vectors',
                            'nonce_vectors', 'excluded_case_classes'))
    if doc['fixture_kind'] != 'team-context-v1/pseudonyms':
        raise FixtureError('pseudonyms.json: unexpected fixture_kind')
    if doc['fixture_version'] != 1:
        raise FixtureError('pseudonyms.json: unsupported fixture_version')
    require_bool_true(doc, 'non_secret', 'pseudonyms.json')

    pepper = check_members(doc['pepper'], 'pseudonyms.json pepper',
                           required=('id', 'hex', 'bit_length', 'derivation',
                                     'non_secret', 'warning'))
    require_bool_true(pepper, 'non_secret', 'pseudonyms.json pepper')
    pepper_hex = require_str(pepper, 'hex', 'pseudonyms.json pepper')
    try:
        pepper_raw = binascii.unhexlify(pepper_hex)
    except (binascii.Error, ValueError):
        raise FixtureError('pseudonyms.json pepper.hex is not hexadecimal')
    if pepper_hex != pepper_hex.lower():
        raise FixtureError('pseudonyms.json pepper.hex must be lowercase')
    bits = len(pepper_raw) * 8
    if require_int(pepper, 'bit_length', 'pseudonyms.json pepper') != bits:
        raise FixtureError('pseudonyms.json pepper.bit_length disagrees with hex')
    if bits < MIN_PEPPER_BITS:
        rep.fail('pepper/bit-length',
                 'pepper is %d bits, contract requires at least %d'
                 % (bits, MIN_PEPPER_BITS))
    else:
        rep.ok('pepper/bit-length', '%d bits, test-only' % bits)

    vectors = doc['pseudonym_vectors']
    if not isinstance(vectors, list) or not vectors:
        raise FixtureError('pseudonyms.json: pseudonym_vectors must be non-empty')
    for i, vec in enumerate(vectors):
        where = 'pseudonyms.json pseudonym_vectors[%d]' % i
        check_members(vec, where, PSEUDO_REQ)
        vid = require_str(vec, 'id', where)
        subject = require_str(vec, 'subject', where)
        sub_b64 = b64u_decode(vec['subject_utf8_base64'],
                              len(subject.encode('utf-8')),
                              '%s subject_utf8_base64' % where)
        if sub_b64 != subject.encode('utf-8'):
            rep.fail('pseudonym/%s' % vid, 'subject_utf8_base64 mismatch')
            continue
        # Contract order: lowercase first, then NFC.
        normalized = unicodedata.normalize('NFC', subject.lower())
        if require_str(vec, 'lowercased_nfc', where) != normalized:
            rep.fail('pseudonym/%s' % vid,
                     'lowercased_nfc mismatch: computed %s'
                     % json.dumps(normalized))
            continue
        norm_bytes = normalized.encode('utf-8')
        if b64u_decode(vec['lowercased_nfc_utf8_base64'], len(norm_bytes),
                       '%s lowercased_nfc_utf8_base64' % where) != norm_bytes:
            rep.fail('pseudonym/%s' % vid, 'lowercased_nfc_utf8_base64 mismatch')
            continue
        mac = hmac.new(pepper_raw, norm_bytes, hashlib.sha256).hexdigest()
        want = require_str(vec, 'hmac_sha256', where)
        if want != 'hmac-sha256:' + mac:
            rep.fail('pseudonym/%s' % vid,
                     'HMAC mismatch: computed hmac-sha256:%s' % mac)
            continue
        rep.ok('pseudonym/%s' % vid, 'hmac-sha256 reproduced')

    nonces = doc['nonce_vectors']
    if not isinstance(nonces, list) or not nonces:
        raise FixtureError('pseudonyms.json: nonce_vectors must be non-empty')
    for i, vec in enumerate(nonces):
        where = 'pseudonyms.json nonce_vectors[%d]' % i
        check_members(vec, where, NONCE_REQ)
        vid = require_str(vec, 'id', where)
        nhex = require_str(vec, 'nonce_hex', where)
        if nhex != nhex.lower():
            raise FixtureError('%s: nonce_hex must be lowercase' % where)
        try:
            raw = binascii.unhexlify(nhex)
        except (binascii.Error, ValueError):
            raise FixtureError('%s: nonce_hex is not hexadecimal' % where)
        nbits = len(raw) * 8
        if require_int(vec, 'nonce_bit_length', where) != nbits:
            rep.fail('nonce/%s' % vid, 'nonce_bit_length disagrees with nonce_hex')
            continue
        if nbits < MIN_NONCE_BITS:
            rep.fail('nonce/%s' % vid,
                     'nonce is %d bits, contract requires at least %d'
                     % (nbits, MIN_NONCE_BITS))
            continue
        digest = 'sha256:' + hashlib.sha256(raw).hexdigest()
        if require_str(vec, 'sha256', where) != digest:
            rep.fail('nonce/%s' % vid, 'sha256 mismatch: computed %s' % digest)
            continue
        rep.ok('nonce/%s' % vid, '%d-bit preimage' % nbits)

    excluded = doc['excluded_case_classes']
    if not isinstance(excluded, list) or not excluded:
        raise FixtureError('pseudonyms.json: excluded_case_classes must be non-empty')
    for i, item in enumerate(excluded):
        check_members(item, 'pseudonyms.json excluded_case_classes[%d]' % i,
                      required=('id', 'reason'))
    rep.ok('pseudonym/excluded-classes',
           '%d locale- or context-sensitive class(es) documented' % len(excluded))

    rep.count('pseudonym_vectors', len(vectors))
    rep.count('nonce_vectors', len(nonces))


# --------------------------------------------------------------------------
# Cross-record rules
# --------------------------------------------------------------------------

RECORD_TYPES = frozenset([
    'authority-decision', 'checkpoint', 'conflict', 'context-object', 'invite',
    'lease', 'member', 'membership', 'organization', 'policy', 'promotion',
    'provenance', 'signature-envelope', 'team-receipt', 'workspace',
    'workspace-role',
])

RULE_NAMES = {
    1: 'Non-self-approval',
    2: 'Same-scope approval',
    3: 'Approver authority',
    4: 'Signer binding',
    5: 'Policy agreement',
    6: 'Object linkage',
    7: 'Conflict winner',
    8: 'Validity ordering',
    9: 'Provenance agreement',
    10: 'Lease derivation',
    11: 'CAS',
    12: 'Array canonicalization',
}

ORDERED_ARRAY_FIELDS = ('object_digests', 'receipt_digests', 'allowed_actions')


class Bundle(object):
    """A self-contained set of records plus a digest index over them."""

    def __init__(self, records, tenant):
        self.records = records                    # label -> record dict
        self.types = {}                           # label -> schema type
        self.by_digest = {}                       # digest -> [label, ...]
        self.tenant = tenant

    def get(self, label):
        return self.records.get(label)

    def resolve(self, digest):
        """Return the single record with this digest, or None."""
        labels = self.by_digest.get(digest)
        if not labels or len(labels) != 1:
            return None
        return self.records[labels[0]]

    def resolve_typed(self, digest, type_name):
        labels = self.by_digest.get(digest)
        if not labels or len(labels) != 1:
            return None
        label = labels[0]
        return self.records[label] if self.types[label] == type_name else None

    def type_of(self, record):
        for label in sorted(self.records):
            if self.records[label] is record:
                return self.types[label]
        return None

    def of_type(self, type_name):
        return [self.records[k] for k in sorted(self.records)
                if self.types[k] == type_name]


def scopes_equal(a, b):
    return isinstance(a, dict) and isinstance(b, dict) and a == b


def _rule1(b, v):
    dec = b.get('decision')
    prom1 = b.get('promotion_v1')
    prom2 = b.get('promotion_v2')
    if dec is None:
        return
    if prom1 is not None and dec.get('decided_by') == prom1.get('requested_by'):
        v(1, 'decided_by %s equals the promotion requester' % dec.get('decided_by'))
    if prom1 is not None and prom1.get('decided_by') is not None:
        if prom1.get('decided_by') != dec.get('decided_by'):
            v(1, 'promotion.decided_by %s != authority-decision.decided_by %s'
              % (prom1.get('decided_by'), dec.get('decided_by')))
    promotion_ref = dec.get('promotion_digest')
    if promotion_ref is not None:
        linked = b.resolve_typed(promotion_ref, 'promotion')
        if linked is None or linked.get('promotion_id') != dec.get('promotion_id'):
            v(1, 'authority-decision promotion link is missing, mistyped or misidentified')
    if prom2 is not None and prom2.get('decided_by') is not None:
        if prom2.get('decided_by') != dec.get('decided_by'):
            v(1, 'promotion.decided_by %s != authority-decision.decided_by %s'
              % (prom2.get('decided_by'), dec.get('decided_by')))


def _rule2(b, v):
    dec = b.get('decision')
    if dec is None:
        return
    for label in ('promotion_v1', 'promotion_v2', 'policy', 'object_v2', 'object_v3'):
        other = b.get(label)
        if other is None:
            continue
        if not scopes_equal(dec.get('scope'), other.get('scope')):
            v(2, 'authority-decision scope differs from %s scope' % label)


def _rule3(b, v):
    dec = b.get('decision')
    ms = b.get('membership')
    rb = b.get('role_binding')
    if dec is None:
        return
    decided_at = dec.get('decided_at')
    if ms is None:
        v(3, 'no membership record backing decided_by')
    else:
        if ms.get('member_id') != dec.get('decided_by'):
            v(3, 'membership member_id %s does not back decided_by %s'
              % (ms.get('member_id'), dec.get('decided_by')))
        if ms.get('state') != 'active':
            v(3, 'membership state is %r, not active' % ms.get('state'))
        if not scopes_equal(ms.get('scope'), dec.get('scope')):
            v(3, 'membership scope differs from decision scope')
        if not _covers(ms, decided_at):
            v(3, 'membership validity window does not cover decided_at')
    if rb is None:
        v(3, 'no workspace-role binding granting promotion.decide')
    else:
        if rb.get('state') != 'active':
            v(3, 'role binding state is %r, not active' % rb.get('state'))
        if not scopes_equal(rb.get('scope'), dec.get('scope')):
            v(3, 'role binding scope differs from decision scope')
        actions = rb.get('allowed_actions') or []
        if 'promotion.decide' not in actions:
            v(3, 'role binding does not grant promotion.decide')
        if ms is not None and rb.get('role') != ms.get('role'):
            v(3, 'role binding role %r does not match membership role %r'
              % (rb.get('role'), ms.get('role')))
        if not _covers(rb, decided_at):
            v(3, 'role binding validity window does not cover decided_at')


def _covers(record, instant):
    """True when `instant` lies inside [valid_from, valid_until). Timestamps are
    canonical UTC with a fixed grammar, so lexicographic comparison is correct
    only when the fractional-second form matches; the fixtures use whole
    seconds throughout, which the fixture loader enforces."""
    if instant is None:
        return True
    vf = record.get('valid_from')
    vu = record.get('valid_until')
    if vf is not None and instant < vf:
        return False
    if vu is not None and instant >= vu:
        return False
    return True


def _rule4(b, v):
    for rec_label, env_label, kind in (
            ('decision', 'decision_envelope', 'authority-decision'),
            ('policy', 'policy_envelope', 'policy'),
            ('receipt', 'receipt_envelope', 'team-receipt')):
        rec = b.get(rec_label)
        if rec is None:
            continue
        ref = rec.get('signature_envelope_digest')
        if ref is None:
            v(4, '%s carries no signature_envelope_digest' % rec_label)
            continue
        env = b.resolve_typed(ref, 'signature-envelope')
        if env is None:
            v(4, '%s.signature_envelope_digest does not resolve to a bundled '
                 'signature-envelope' % rec_label)
            continue
        if env.get('signed_payload_kind') != kind:
            v(4, '%s envelope signed_payload_kind is %r, expected %r'
              % (rec_label, env.get('signed_payload_kind'), kind))
        computed = preimage_digest(rec)
        if env.get('signed_payload_digest') != computed:
            v(4, '%s envelope signed_payload_digest does not match the record '
                 'digest computed with signature_envelope_digest omitted'
              % rec_label)
            continue
        if rec_label == 'receipt':
            expected_signer = rec.get('actor_digest')
            if (env.get('signed_by_digest') != expected_signer
                    or env.get('signed_by') is not None):
                v(4, 'receipt envelope signer digest does not match actor_digest')
        else:
            expected_signer = (rec.get('decided_by') if rec_label == 'decision'
                               else rec.get('created_by'))
            if (env.get('signed_by') != expected_signer
                    or env.get('signed_by_digest') is not None):
                v(4, '%s envelope signer does not match the record principal'
                  % rec_label)
        min_epoch = b.tenant.get('min_key_epoch')
        if min_epoch is not None and env.get('key_epoch', 0) < min_epoch:
            v(4, '%s envelope key_epoch %s is below the tenant minimum %s'
              % (rec_label, env.get('key_epoch'), min_epoch))
        try:
            pub = b64u_decode(env.get('public_key'), 32, 'public_key')
            sig = b64u_decode(env.get('signature'), 64, 'signature')
        except ValueError as exc:
            v(4, '%s envelope key or signature is malformed: %s' % (rec_label, exc))
            continue
        msg = ('leanctx-team-context-v1:%s:%s'
               % (env.get('signed_payload_kind'),
                  env.get('signed_payload_digest'))).encode('ascii')
        if not ed25519_verify(pub, msg, sig):
            v(4, '%s envelope Ed25519 signature does not verify' % rec_label)


def _rule5(b, v):
    dec = b.get('decision')
    prom = b.get('promotion_v1')
    pol = b.get('policy')
    if dec is None:
        return
    if prom is not None and prom.get('policy_digest') != dec.get('policy_digest'):
        v(5, 'promotion.policy_digest != authority-decision.policy_digest')
    ref = dec.get('policy_digest')
    resolved = b.resolve_typed(ref, 'policy') if ref else None
    if resolved is None:
        v(5, 'authority-decision.policy_digest does not resolve to a bundled policy')
        return
    if pol is not None and resolved is not pol:
        v(5, 'authority-decision.policy_digest resolves to a record other than '
             'the bundled policy')
    if resolved.get('state') != 'active':
        v(5, 'referenced policy state is %r, not active' % resolved.get('state'))
    if resolved.get('policy_kind') != 'promotion_authority':
        v(5, 'referenced policy kind is %r, not promotion_authority'
          % resolved.get('policy_kind'))
    if not scopes_equal(resolved.get('scope'), dec.get('scope')):
        v(5, 'referenced policy scope differs from decision scope')
    if not _covers(resolved, dec.get('decided_at')):
        v(5, 'referenced policy is outside its validity window at decided_at')


def _rule6(b, v):
    obj = b.get('object_v3')
    if obj is None:
        return
    if obj.get('authority_state') != 'authoritative':
        return
    ref = obj.get('authority_decision_digest')
    if ref is None:
        v(6, 'authoritative object carries no authority_decision_digest')
        return
    dec = b.resolve_typed(ref, 'authority-decision')
    if dec is None:
        v(6, 'authority_decision_digest does not resolve to a bundled '
             'authority-decision')
        return
    if dec.get('decision') != 'approved':
        v(6, 'referenced authority-decision decision is %r, not approved'
          % dec.get('decision'))
    if dec.get('object_id') != obj.get('object_id'):
        v(6, 'authority-decision object_id %r != object %r'
          % (dec.get('object_id'), obj.get('object_id')))
    approved = (b.resolve_typed(dec.get('object_digest'), 'context-object')
                if dec.get('object_digest') else None)
    if approved is None:
        v(6, 'authority-decision.object_digest does not resolve to a bundled '
             'context-object version')
    elif approved.get('object_id') != obj.get('object_id'):
        v(6, 'authority-decision.object_digest resolves to a different object')
    elif obj.get('parent_digest') != dec.get('object_digest'):
        v(6, 'the approved object version is not the immediate predecessor of '
             'the authoritative version')


def _rule7(b, v):
    cf = b.get('conflict')
    if cf is None:
        return
    winner = cf.get('winner')
    if cf.get('state') == 'resolved':
        if winner not in ('left', 'right'):
            v(7, 'resolved conflict winner is %r, not left or right' % winner)
    for side in ('left', 'right'):
        ref = cf.get('%s_digest' % side)
        branch = b.resolve_typed(ref, 'context-object') if ref else None
        if branch is None:
            v(7, '%s_digest does not resolve to a bundled context-object' % side)
            continue
        if branch.get('object_id') != cf.get('%s_object_id' % side):
            v(7, '%s_digest resolves to object %r but %s_object_id is %r'
              % (side, branch.get('object_id'), side, cf.get('%s_object_id' % side)))
        if branch.get('authority_state') not in ('authoritative', 'team_candidate'):
            v(7, '%s branch authority_state is %r; a conflict branch must be '
                 'authoritative or team_candidate'
              % (side, branch.get('authority_state')))
        if not scopes_equal(branch.get('scope'), cf.get('scope')):
            v(7, '%s branch scope differs from the conflict scope' % side)


VALIDITY_PAIRS = (
    ('valid_from', 'valid_until', False),
    ('granted_at', 'expires_at', False),
    ('created_at', 'expires_at', False),
)


def _rule8(b, v):
    for label in sorted(b.records):
        rec = b.records[label]
        for lo_key, hi_key, allow_equal in VALIDITY_PAIRS:
            lo = rec.get(lo_key)
            hi = rec.get(hi_key)
            if isinstance(lo, str) and isinstance(hi, str):
                if hi < lo or (hi == lo and not allow_equal):
                    v(8, '%s: %s (%s) is not strictly after %s (%s)'
                      % (label, hi_key, hi, lo_key, lo))
        created = rec.get('created_at') or rec.get('granted_at')
        revoked = rec.get('revoked_at')
        if isinstance(created, str) and isinstance(revoked, str) and revoked < created:
            v(8, '%s: revoked_at (%s) precedes created_at (%s)'
              % (label, revoked, created))


PROVENANCE_MIRRORED = ('object_id', 'authority_state', 'valid_from',
                       'valid_until', 'classification')


def _rule9(b, v):
    for label in sorted(b.records):
        if b.types[label] != 'provenance':
            continue
        prov = b.records[label]
        candidates = [obj for obj in b.of_type('context-object')
                      if provenance_subject_digest(obj) == prov.get('object_digest')]
        if len(candidates) != 1:
            v(9, '%s.object_digest does not identify exactly one bundled '
              'context-object omission preimage' % label)
            continue
        subject = candidates[0]
        if subject.get('provenance_digest') != record_digest(prov):
            v(9, '%s is not referenced by its subject provenance_digest' % label)
        for field in PROVENANCE_MIRRORED:
            if prov.get(field) != subject.get(field):
                v(9, '%s.%s (%r) differs from the subject object version (%r)'
                  % (label, field, prov.get(field), subject.get(field)))


def _rule10(b, v):
    memberships = {}
    bindings = {}
    for label in sorted(b.records):
        rec = b.records[label]
        if b.types[label] == 'membership':
            memberships[record_digest(rec)] = rec
        elif b.types[label] == 'workspace-role':
            bindings[record_digest(rec)] = rec
    for label in sorted(b.records):
        if b.types[label] != 'lease':
            continue
        lease = b.records[label]
        ms = memberships.get(lease.get('membership_digest'))
        rb = bindings.get(lease.get('role_binding_digest'))
        if ms is None:
            v(10, '%s.membership_digest does not resolve to a bundled membership'
              % label)
        if rb is None:
            v(10, '%s.role_binding_digest does not resolve to a bundled '
                  'workspace-role' % label)
        if ms is not None and ms.get('state') != 'active':
            v(10, '%s membership source is %r, not active'
              % (label, ms.get('state')))
        if rb is not None and rb.get('state') != 'active':
            v(10, '%s role-binding source is %r, not active'
              % (label, rb.get('state')))
        if ms is not None and not scopes_equal(ms.get('scope'), lease.get('scope')):
            v(10, '%s scope differs from its membership scope' % label)
        if rb is not None and not scopes_equal(rb.get('scope'), lease.get('scope')):
            v(10, '%s scope differs from its role binding scope' % label)
        granted_at = lease.get('granted_at')
        if ms is not None and not _covers(ms, granted_at):
            v(10, '%s membership validity does not cover granted_at' % label)
        if rb is not None and not _covers(rb, granted_at):
            v(10, '%s role-binding validity does not cover granted_at' % label)
        # Revocation of either derivation source must invalidate the lease.
        if lease.get('state') == 'active':
            if ms is not None and ms.get('state') == 'revoked':
                v(10, '%s is still active although its membership is revoked'
                  % label)
            if rb is not None and rb.get('state') == 'revoked':
                v(10, '%s is still active although its role binding is revoked'
                  % label)
        if lease.get('state') == 'revoked':
            for key in ('revoked_at', 'revoked_by'):
                if lease.get(key) is None:
                    v(10, '%s is revoked but carries no %s' % (label, key))


def _rule11(b, v):
    # Group records by logical identity so predecessors can be located.
    ID_FIELDS = ('object_id', 'promotion_id', 'membership_id', 'role_binding_id',
                 'lease_id', 'policy_id', 'decision_id', 'conflict_id',
                 'invite_id', 'checkpoint_id', 'provenance_id', 'receipt_id',
                 'envelope_id', 'member_id', 'workspace_id', 'organization_id')
    families = {}
    for label in sorted(b.records):
        rec = b.records[label]
        key = None
        for field in ID_FIELDS:
            if field in rec:
                key = (b.types[label], field, rec[field])
                break
        families.setdefault(key, []).append(label)

    for label in sorted(b.records):
        rec = b.records[label]
        version = rec.get('version')
        parent = rec.get('parent_digest')
        # Reduced cross-record fixtures may omit schema-only fields. Schema
        # validation is a separate gate; CAS applies only when version is present.
        if version is None:
            continue
        if version == 1:
            if parent is not None:
                v(11, '%s is version 1 (genesis) but carries parent_digest' % label)
            continue
        if parent is None:
            v(11, '%s is version %r but carries no parent_digest'
              % (label, version))
            continue
        pred = b.resolve(parent)
        if pred is None:
            v(11, '%s.parent_digest does not resolve to a bundled predecessor'
              % label)
            continue
        if not isinstance(version, int) or pred.get('version') != version - 1:
            v(11, '%s version %r does not follow its predecessor version %r'
              % (label, version, pred.get('version')))
            continue
        if b.type_of(pred) != b.types[label]:
            v(11, '%s predecessor has a different record type' % label)
            continue
        id_fields = ('object_id', 'promotion_id', 'membership_id',
                     'role_binding_id', 'lease_id', 'policy_id', 'decision_id',
                     'conflict_id', 'invite_id', 'checkpoint_id',
                     'provenance_id', 'receipt_id', 'envelope_id', 'member_id',
                     'workspace_id', 'organization_id')
        identity_field = next((field for field in id_fields if field in rec), None)
        if (identity_field is None or pred.get(identity_field) != rec.get(identity_field)):
            v(11, '%s predecessor has a different logical identity' % label)
            continue
        if ('scope' in rec or 'scope' in pred) and not scopes_equal(
                rec.get('scope'), pred.get('scope')):
            v(11, '%s predecessor has a different scope' % label)


def _rule12(b, v):
    for label in sorted(b.records):
        rec = b.records[label]
        for field in ORDERED_ARRAY_FIELDS:
            arr = rec.get(field)
            if arr is None:
                continue
            if not isinstance(arr, list):
                v(12, '%s.%s is not an array' % (label, field))
                continue
            if any(not isinstance(x, str) for x in arr):
                v(12, '%s.%s contains a non-string element' % (label, field))
                continue
            if len(set(arr)) != len(arr):
                v(12, '%s.%s contains duplicate entries' % (label, field))
            if list(arr) != sorted(arr):
                v(12, '%s.%s is not in ascending lexicographic order'
                  % (label, field))


RULE_CHECKS = {1: _rule1, 2: _rule2, 3: _rule3, 4: _rule4, 5: _rule5,
               6: _rule6, 7: _rule7, 8: _rule8, 9: _rule9, 10: _rule10,
               11: _rule11, 12: _rule12}


def evaluate(bundle):
    """Run all 12 rule checkers. Returns (violated_rule_numbers, messages)."""
    violated = set()
    messages = []

    def make(rule):
        def record_violation(rule_no, message):
            violated.add(rule_no)
            messages.append((rule_no, message))
        return record_violation

    for rule in sorted(RULE_CHECKS):
        RULE_CHECKS[rule](bundle, make(rule))
    messages.sort()
    return violated, messages


CASE_REQ = ('case_id', 'rule', 'expect', 'description', 'tenant', 'record_set')
CASE_OPT = ('expected_violations', 'mutations')
SUPP_REQ = ('case_id', 'topic', 'expect', 'description', 'operations')
OP_REQ = ('label', 'scope', 'operation_kind', 'idempotency_key', 'request_body')


def build_bundle(case_records, tenant, where, schemas):
    if not isinstance(case_records, dict) or not case_records:
        raise FixtureError('%s.records: expected a non-empty object' % where)
    records = {}
    types = {}
    for label in sorted(case_records):
        entry = check_members(case_records[label], '%s.records.%s' % (where, label),
                              required=('type', 'record'))
        type_name = require_str(entry, 'type', '%s.records.%s' % (where, label))
        if type_name not in RECORD_TYPES:
            raise FixtureError('%s.records.%s: unknown record type %r'
                               % (where, label, type_name))
        record = require_object(entry['record'], '%s.records.%s.record'
                                % (where, label))
        if record.get('v') != 1 or isinstance(record.get('v'), bool):
            raise FixtureError('%s.records.%s.record: v must be 1'
                               % (where, label))
        schema_name = type_name + '.schema.json'
        validation = schema_errors(record, schemas[schema_name], schemas,
                                   schema_name, '%s.records.%s.record'
                                   % (where, label))
        if validation:
            raise FixtureError('%s.records.%s.record is not schema-valid: %s'
                               % (where, label, '; '.join(validation[:5])))
        try:
            canonical_bytes(record)
        except JcsError as exc:
            raise FixtureError('%s.records.%s.record is not canonicalizable: %s'
                               % (where, label, exc))
        for key in ('created_at', 'valid_from', 'valid_until', 'decided_at',
                    'granted_at', 'expires_at', 'requested_at', 'revoked_at',
                    'signed_at', 'sealed_at', 'detected_at', 'resolved_at',
                    'accepted_at'):
            value = record.get(key)
            if value is not None and not _is_whole_second_utc(value):
                raise FixtureError(
                    '%s.records.%s.record.%s (%r) must be a whole-second UTC '
                    'timestamp so ordering comparisons are lexicographic'
                    % (where, label, key, value))
        records[label] = record
        types[label] = type_name
    bundle = Bundle(records, tenant)
    bundle.types = types
    for label in sorted(records):
        bundle.by_digest.setdefault(record_digest(records[label]), []).append(label)
    return bundle


def materialize_records(record_sets, case, where):
    base = require_str(case, 'record_set', where)
    if base not in record_sets:
        raise FixtureError('%s.record_set: unknown set %r' % (where, base))
    records = copy.deepcopy(record_sets[base])
    mutations = case.get('mutations', [])
    if not isinstance(mutations, list):
        raise FixtureError('%s.mutations: expected an array' % where)
    for index, mutation in enumerate(mutations):
        mwhere = '%s.mutations[%d]' % (where, index)
        mutation = require_object(mutation, mwhere)
        op = require_str(mutation, 'op', mwhere)
        label = require_str(mutation, 'label', mwhere)
        if label not in records:
            raise FixtureError('%s.label: unknown record %r' % (mwhere, label))
        if op == 'delete_record':
            check_members(mutation, mwhere, required=('op', 'label'))
            del records[label]
        elif op == 'set':
            check_members(mutation, mwhere,
                          required=('op', 'label', 'field', 'value'))
            field = require_str(mutation, 'field', mwhere)
            records[label]['record'][field] = mutation['value']
        elif op == 'delete':
            check_members(mutation, mwhere, required=('op', 'label', 'field'))
            field = require_str(mutation, 'field', mwhere)
            if field not in records[label]['record']:
                raise FixtureError('%s.field: absent member %r' % (mwhere, field))
            del records[label]['record'][field]
        else:
            raise FixtureError('%s.op: unsupported operation %r' % (mwhere, op))
    return records


def _is_whole_second_utc(value):
    if not isinstance(value, str) or len(value) != 20:
        return False
    return (value[4] == value[7] == '-' and value[10] == 'T'
            and value[13] == value[16] == ':' and value[19] == 'Z'
            and value[:4].isdigit() and value[5:7].isdigit()
            and value[8:10].isdigit() and value[11:13].isdigit()
            and value[14:16].isdigit() and value[17:19].isdigit())


def verify_cases(path, rep):
    doc = load_json(path)
    schemas = load_contract_schemas(os.path.dirname(os.path.dirname(path)))
    check_members(doc, 'cross-record-cases.json',
                  required=('fixture_kind', 'fixture_version', 'non_secret',
                            'description', 'interpretations', 'rules', 'record_sets', 'cases',
                            'supplementary_cases'))
    if doc['fixture_kind'] != 'team-context-v1/cross-record-cases':
        raise FixtureError('cross-record-cases.json: unexpected fixture_kind')
    if doc['fixture_version'] != 1:
        raise FixtureError('cross-record-cases.json: unsupported fixture_version')
    require_bool_true(doc, 'non_secret', 'cross-record-cases.json')

    rules = doc['rules']
    if not isinstance(rules, list):
        raise FixtureError('cross-record-cases.json: rules must be an array')
    declared = []
    for i, item in enumerate(rules):
        where = 'cross-record-cases.json rules[%d]' % i
        check_members(item, where, required=('rule', 'name', 'summary'))
        number = require_int(item, 'rule', where)
        name = require_str(item, 'name', where)
        if RULE_NAMES.get(number) != name:
            raise FixtureError('%s: rule %d is named %r, README calls it %r'
                               % (where, number, name, RULE_NAMES.get(number)))
        declared.append(number)
    if sorted(declared) != list(range(1, 13)):
        raise FixtureError('cross-record-cases.json: rules must declare exactly '
                           'rules 1..12, got %s' % sorted(declared))
    rep.ok('inventory/rule-declarations', 'rules 1..12 declared exactly once')

    record_sets = require_object(doc['record_sets'],
                                 'cross-record-cases.json.record_sets')
    if not record_sets:
        raise FixtureError('cross-record-cases.json.record_sets: expected non-empty object')

    cases = doc['cases']
    if not isinstance(cases, list) or not cases:
        raise FixtureError('cross-record-cases.json: cases must be a non-empty array')

    covered_accept = set()
    covered_reject = set()
    seen_ids = set()
    for i, case in enumerate(cases):
        where = 'cross-record-cases.json cases[%d]' % i
        check_members(case, where, CASE_REQ, CASE_OPT)
        case_id = require_str(case, 'case_id', where)
        if case_id in seen_ids:
            raise FixtureError('%s: duplicate case_id %r' % (where, case_id))
        seen_ids.add(case_id)
        rule = require_int(case, 'rule', where)
        if rule not in RULE_NAMES:
            raise FixtureError('%s: rule %d is outside 1..12' % (where, rule))
        expect = require_str(case, 'expect', where)
        if expect not in ('accept', 'reject'):
            raise FixtureError('%s: expect must be "accept" or "reject"' % where)
        tenant = check_members(case['tenant'], '%s.tenant' % where,
                               required=('min_key_epoch',))
        require_int(tenant, 'min_key_epoch', '%s.tenant' % where)

        if expect == 'reject':
            if 'expected_violations' not in case:
                raise FixtureError('%s: a reject case must declare '
                                   'expected_violations' % where)
            expected = case['expected_violations']
            if (not isinstance(expected, list) or not expected
                    or any(not isinstance(x, int) or isinstance(x, bool)
                           or x not in RULE_NAMES for x in expected)):
                raise FixtureError('%s: expected_violations must be a non-empty '
                                   'array of rule numbers in 1..12' % where)
            if rule not in expected:
                raise FixtureError('%s: expected_violations must contain the '
                                   "case's own rule %d" % (where, rule))
            expected_set = set(expected)
            covered_reject.add(rule)
        else:
            if 'expected_violations' in case:
                raise FixtureError('%s: an accept case must not declare '
                                   'expected_violations' % where)
            expected_set = set()
            covered_accept.add(rule)

        case_records = materialize_records(record_sets, case, where)
        bundle = build_bundle(case_records, tenant, where, schemas)
        violated, messages = evaluate(bundle)

        if violated == expected_set:
            if expect == 'accept':
                rep.ok('case/%s' % case_id, 'accepted, no rule violated')
            else:
                rep.ok('case/%s' % case_id,
                       'rejected by rule(s) %s'
                       % ','.join(str(r) for r in sorted(violated)))
        else:
            missing = sorted(expected_set - violated)
            extra = sorted(violated - expected_set)
            detail = []
            if missing:
                detail.append('rule(s) %s did not fire'
                              % ','.join(str(r) for r in missing))
            if extra:
                detail.append('unexpected violation of rule(s) %s: %s'
                              % (','.join(str(r) for r in extra),
                                 '; '.join(m for r, m in messages
                                           if r in extra)))
            rep.fail('case/%s' % case_id, ' | '.join(detail))

    missing_accept = sorted(set(range(1, 13)) - covered_accept)
    missing_reject = sorted(set(range(1, 13)) - covered_reject)
    if missing_accept:
        rep.fail('inventory/accept-coverage',
                 'no accept case for rule(s) %s'
                 % ','.join(str(r) for r in missing_accept))
    else:
        rep.ok('inventory/accept-coverage', 'every rule 1..12 has an accept case')
    if missing_reject:
        rep.fail('inventory/reject-coverage',
                 'no reject case for rule(s) %s'
                 % ','.join(str(r) for r in missing_reject))
    else:
        rep.ok('inventory/reject-coverage', 'every rule 1..12 has a reject case')

    verify_supplementary(doc['supplementary_cases'], rep)
    rep.count('cross_record_cases', len(cases))
    rep.count('supplementary_cases', len(doc['supplementary_cases']))


def verify_supplementary(cases, rep):
    """Idempotency scoping: same (organization, workspace, operation kind,
    idempotency key) must bind identical canonical request bytes."""
    if not isinstance(cases, list) or not cases:
        raise FixtureError('cross-record-cases.json: supplementary_cases must '
                           'be a non-empty array')
    seen = set()
    for i, case in enumerate(cases):
        where = 'cross-record-cases.json supplementary_cases[%d]' % i
        check_members(case, where, SUPP_REQ)
        case_id = require_str(case, 'case_id', where)
        if case_id in seen:
            raise FixtureError('%s: duplicate case_id %r' % (where, case_id))
        seen.add(case_id)
        topic = require_str(case, 'topic', where)
        if topic != 'idempotency':
            raise FixtureError('%s: unknown topic %r' % (where, topic))
        expect = require_str(case, 'expect', where)
        if expect not in ('accept', 'reject'):
            raise FixtureError('%s: expect must be "accept" or "reject"' % where)
        ops = case['operations']
        if not isinstance(ops, list) or len(ops) < 2:
            raise FixtureError('%s: operations must list at least two operations'
                               % where)
        index = {}
        drift = []
        for j, op in enumerate(ops):
            opwhere = '%s.operations[%d]' % (where, j)
            check_members(op, opwhere, OP_REQ)
            scope = require_object(op['scope'], '%s.scope' % opwhere)
            key = (scope.get('organization_id'), scope.get('workspace_id'),
                   require_str(op, 'operation_kind', opwhere),
                   require_str(op, 'idempotency_key', opwhere))
            body = require_object(op['request_body'], '%s.request_body' % opwhere)
            try:
                digest = 'sha256:' + hashlib.sha256(
                    canonical_bytes(body)).hexdigest()
            except JcsError as exc:
                raise FixtureError('%s.request_body: %s' % (opwhere, exc))
            if key in index and index[key][1] != digest:
                drift.append('%s vs %s' % (index[key][0],
                                           require_str(op, 'label', opwhere)))
            index.setdefault(key, (require_str(op, 'label', opwhere), digest))
        rejected = bool(drift)
        if rejected == (expect == 'reject'):
            rep.ok('idempotency/%s' % case_id,
                   'byte drift detected' if rejected
                   else 'replay is byte-identical under JCS')
        else:
            rep.fail('idempotency/%s' % case_id,
                     'expected %s but drift=%s (%s)'
                     % (expect, rejected, '; '.join(drift) or 'none'))


# --------------------------------------------------------------------------

FIXTURES = (
    ('canonicalization.json', verify_canonicalization, 'Canonicalization (RFC 8785 / JCS)'),
    ('pseudonyms.json', verify_pseudonyms, 'Pseudonyms and nonces'),
    ('cross-record-cases.json', verify_cases, 'Cross-record rules 1..12'),
)

DEFAULT_FIXTURES = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    'docs', 'contracts', 'team-context-v1', 'fixtures')


def main(argv):
    parser = argparse.ArgumentParser(
        description='Verify the team-context-v1 contract fixtures.')
    parser.add_argument('--fixtures', default=DEFAULT_FIXTURES,
                        help='fixtures directory (default: %(default)s)')
    args = parser.parse_args(argv)

    rep = Report()
    rep.lines.append('team-context-v1 fixture conformance verifier')
    rep.lines.append('scope: fixture and contract conformance only; this is not '
                     'a JSON Schema validator and')
    rep.lines.append('       proves nothing about runtime or PostgreSQL '
                     'enforcement.')

    rep.section('Self-test')
    if _ed25519_selftest():
        rep.ok('ed25519/rfc8032-vectors', 'TEST 1 and TEST 2 verify, tamper rejected')
    else:
        rep.fail('ed25519/rfc8032-vectors',
                 'the built-in Ed25519 implementation failed RFC 8032 vectors')

    rep.section('Immutable contract pack')
    try:
        verify_frozen_hashes(os.path.dirname(args.fixtures), rep)
    except FixtureError as exc:
        rep.fail('freeze/frozen-hashes.json', str(exc))

    for filename, handler, title in FIXTURES:
        rep.section(title)
        path = os.path.join(args.fixtures, filename)
        if not os.path.isfile(path):
            rep.fail('fixture/%s' % filename, 'missing')
            continue
        try:
            handler(path, rep)
        except FixtureError as exc:
            rep.fail('fixture/%s' % filename, str(exc))
        except (JcsError, ValueError) as exc:
            rep.fail('fixture/%s' % filename, 'unexpected error: %s' % exc)

    rep.section('Summary')
    for key in sorted(rep.counts):
        rep.lines.append('  %-32s %d' % (key, rep.counts[key]))
    rep.lines.append('  %-32s %d' % ('failures', len(rep.failures)))
    rep.lines.append('')
    rep.lines.append('RESULT: %s' % ('PASS' if not rep.failures else 'FAIL'))

    sys.stdout.write('\n'.join(rep.lines) + '\n')
    return 0 if not rep.failures else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
