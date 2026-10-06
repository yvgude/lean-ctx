# SPDX-License-Identifier: Apache-2.0
"""Actual direct CLI context commands under a local content policy; no model calls."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', type=Path, required=True)
    parser.add_argument('--previous-engine', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    engine = args.engine.resolve(strict=True)
    report = dict(passed=False, cases=[], model_transferred=False,
                  engine_sha256=hashlib.sha256(engine.read_bytes()).hexdigest())
    try:
        with tempfile.TemporaryDirectory(prefix='leanctx-cli-policy-family-') as temporary:
            base = Path(temporary).resolve()
            project = base/'project'; project.mkdir(mode=0o700)
            data = base/'data'; data.mkdir(mode=0o700)
            home = base/'home'; home.mkdir(mode=0o700)
            config = base/'config'; config.mkdir(mode=0o700)
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(('LEAN_CTX', 'LEANCTX', '__LEAN_CTX', 'XDG_'))}
            env.update(HOME=str(home), LEAN_CTX_DATA_DIR=str(data),
                       LEAN_CTX_CONFIG_DIR=str(config), DO_NOT_TRACK='1', LEAN_CTX_AUTONOMY='0')

            def run(name, argv, *, expect=0, binary=engine, legacy=False):
                start = time.monotonic()
                call_env = env.copy()
                if legacy:
                    legacy_data = base/('legacy-'+name)
                    legacy_data.mkdir(mode=0o700)
                    call_env.update(LEAN_CTX_DATA_DIR=str(legacy_data), __LEAN_CTX_NO_DAEMON='1')
                result = subprocess.run([str(binary), *argv], cwd=project, env=call_env,
                                        capture_output=True, text=True, timeout=30)
                report['cases'].append(dict(name=name, exit=result.returncode,
                    elapsed_ms=round((time.monotonic()-start)*1000, 2),
                    stdout_sha256=hashlib.sha256(result.stdout.encode()).hexdigest()))
                assert result.returncode == expect, (name, result.returncode)
                return result.stdout + result.stderr

            (project/'before.txt').write_text('public old K-482193\n')
            (project/'after.txt').write_text('public new K-482194\n')
            (project/'blocked.txt').write_text('public STOP-123456\n')
            (project/'useful.py').write_text('def public_function(): pass\n')
            (project/'K-482195.txt').write_text('public file\n')
            (project/'requirements.txt').write_text('public-library==1.0\nprivate-K-482196==2.0\n')
            outside = base/'outside.txt'; outside.write_text('ESCAPE_CANARY\n')
            policy = project/'.lean-ctx/policy.toml'; policy.parent.mkdir(mode=0o700)
            assert 'K-482193' in run('community-read-without-policy', ['read','before.txt','--mode','full'], legacy=True)
            rules = ('name="cli-family"\nversion="1.0.0"\ndescription="fixture"\n'
                     '[redaction]\ncustomer="K-[0-9]{6}"\n'
                     '[filters.blocked_patterns]\nblocked="STOP-[0-9]{6}"\n')
            policy.write_text(rules)
            if args.previous_engine:
                previous = args.previous_engine.resolve(strict=True)
                leaked = run('previous-diff-gap', ['diff', 'before.txt', 'after.txt'], binary=previous, legacy=True)
                assert 'K-482193' in leaked or 'K-482194' in leaked
                report['previous_engine_sha256'] = hashlib.sha256(previous.read_bytes()).hexdigest()
            commands = [
                ('read', ['read', 'before.txt', '--mode', 'full'], 'public'),
                ('diff', ['diff', 'before.txt', 'after.txt'], 'public'),
                ('grep', ['grep', 'public', '.'], 'public'),
                ('glob', ['glob', '*.txt', '.'], 'before.txt'),
                ('find', ['find', 'USEFUL', '.'], 'useful.py'),
                ('ls', ['ls', '.', '--depth', '2'], 'useful.py'),
                ('deps', ['deps', '.'], 'public-library'),
            ]
            for name, argv, useful in commands:
                text = run(name, argv)
                assert useful in text, name
                assert not any(value in text for value in ('K-482193','K-482194','K-482195','K-482196','STOP-123456')), name
            text = run('read-protected-filename', ['read', 'K-482195.txt', '--mode', 'full'])
            assert 'public file' in text and 'K-482195' not in text
            run('grep-only-admitted-text', ['grep', 'K-[0-9]{6}', '.'], expect=1)
            text = run('blocked-diff', ['diff', 'before.txt', 'blocked.txt'], expect=1)
            assert 'STOP-123456' not in text and 'K-482193' not in text
            (project/'STOP-999999.txt').write_text('ordinary\n')
            (project/'.env').write_text('ordinary\n')
            for name, argv in [('glob',['glob','*.txt','.']),('ls',['ls','.','--all'])]:
                text=run('protected-filenames-'+name,argv)
                assert 'STOP-999999' not in text and 'K-482195' not in text and '.env' not in text
            run('find-blocked-filename',['find','STOP','.'],expect=1)
            (project/'requirements.txt').write_text('# STOP-123456\npublic-library==1.0\n')
            text = run('blocked-original-manifest', ['deps', '.'], expect=1)
            assert 'STOP-123456' not in text and 'public-library' not in text
            for name, argv in [('diff',['diff','before.txt',str(outside)]),
                               ('grep',['grep','ESCAPE',str(base)]),
                               ('glob',['glob','*',str(base)]),
                               ('find',['find','outside',str(base)]),
                               ('ls',['ls',str(base)]),('deps',['deps',str(base)])]:
                text = run('outside-'+name, argv, expect=1)
                assert 'ESCAPE_CANARY' not in text
            policy.write_text(rules+'[context]\ndeny_tools=["ctx_read","ctx_search","ctx_glob","ctx_tree"]\n')
            for name, argv, _ in commands:
                text = run('denied-'+name, argv, expect=1)
                assert not any(value in text for value in ('K-482193','K-482194','K-482195','K-482196','STOP-123456')), name
            policy.write_text('invalid = [')
            for name, argv, _ in commands:
                run('invalid-policy-'+name, argv, expect=1)
            for path in data.rglob('*'):
                if path.is_file():
                    payload=path.read_bytes()
                    assert all(value not in payload for value in (b'K-482193',b'K-482194',b'K-482195',b'K-482196',b'STOP-123456',b'STOP-999999',b'ESCAPE_CANARY')), path.name
            report['passed'] = True
    finally:
        args.output.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(dict(passed=True, cases=len(report['cases']))))


if __name__ == '__main__':
    main()
