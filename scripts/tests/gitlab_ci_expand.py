# SPDX-License-Identifier: Apache-2.0
"""Expand the p5 GitLab CI (workflow and job rules, extends, !reference,
variable precedence) into the effective jobs of a pipeline: tags,
resource_group and cache policies included (bd, !520 review).

Used by test_gitlab_runtime_gate.py. As a script it compares two checkouts:
  python3 scripts/tests/gitlab_ci_expand.py <before-root> <after-root>
"""
import copy, json, os, re, sys, itertools
import yaml

class Ref(list):
    pass

class Loader(yaml.SafeLoader):
    pass

def ref_ctor(loader, node):
    return Ref(loader.construct_sequence(node))

Loader.add_constructor('!reference', ref_ctor)

def load(root):
    with open(f'{root}/ci/gitlab-ci.yml', encoding='utf-8') as f:
        d = yaml.load(f, Loader=Loader)
    # The include exists only in the private checkout (rules: exists).
    if os.path.exists(f'{root}/ee/ci/scope-register.yml'):
        with open(f'{root}/ee/ci/scope-register.yml', encoding='utf-8') as f:
            inc = yaml.load(f, Loader=Loader)
        for k, v in inc.items():
            d.setdefault(k, v)
    return d

def deep_merge(a, b):
    if isinstance(a, dict) and isinstance(b, dict):
        out = dict(a)
        for k, v in b.items():
            out[k] = deep_merge(a[k], v) if k in a else copy.deepcopy(v)
        return out
    return copy.deepcopy(b)

def resolve_extends(d, name, seen=()):
    job = d[name]
    ext = job.get('extends')
    if not ext:
        return copy.deepcopy(job)
    exts = ext if isinstance(ext, list) else [ext]
    base = {}
    for e in exts:
        base = deep_merge(base, resolve_extends(d, e, seen + (name,)))
    j = {k: v for k, v in job.items() if k != 'extends'}
    return deep_merge(base, j)

class MissingReference(KeyError):
    """A !reference whose target is not defined in the loaded configuration."""

def resolve_refs(d, node):
    if isinstance(node, Ref):
        cur = d
        for p in node:
            if not isinstance(cur, dict) or p not in cur:
                raise MissingReference(list(node))
            cur = cur[p]
        return resolve_refs(d, copy.deepcopy(cur))
    if isinstance(node, dict):
        return {k: resolve_refs(d, v) for k, v in node.items()}
    if isinstance(node, list):
        out = []
        for x in node:
            r = resolve_refs(d, x)
            # !reference inside a list of rules/script flattens one level
            if isinstance(x, Ref) and isinstance(r, list):
                out.extend(r)
            else:
                out.append(r)
        return out
    return node

def expand(root):
    d = load(root)
    # resolve extends first (GitLab: extends before !reference? both are config-time; order irrelevant here)
    d2 = {}
    for k, v in d.items():
        if isinstance(v, dict) and k not in ('variables', 'workflow', 'include', 'stages', 'default', 'cache'):
            d2[k] = resolve_extends(d, k)
        else:
            d2[k] = v
    d3 = {k: resolve_refs(d2, v) for k, v in d2.items()}
    return d3

# ---- rule expression evaluation ----
TOK = re.compile(r"\s*(\(|\)|&&|\|\||==|!=|=~|!~|\$[A-Za-z_][A-Za-z0-9_]*|\"[^\"]*\"|'[^']*'|/(?:\\/|[^/])*/|null)")

def tokenize(s):
    pos, out = 0, []
    s = s.strip()
    while pos < len(s):
        m = TOK.match(s, pos)
        if not m:
            raise ValueError(f'bad expr at {s[pos:]!r} in {s!r}')
        out.append(m.group(1)); pos = m.end()
    return out

def ev(expr, env):
    toks = tokenize(expr); i = 0
    def val(t):
        if t.startswith('$'): return env.get(t[1:])
        if t.startswith('"') or t.startswith("'"): return t[1:-1]
        if t == 'null': return None
        if t.startswith('/'): return ('re', t[1:-1])
        raise ValueError(t)
    def primary():
        nonlocal i
        t = toks[i]
        if t == '(':
            i += 1; r = orexp(); assert toks[i] == ')'; i += 1; return r
        i += 1; left = val(t)
        if i < len(toks) and toks[i] in ('==', '!=', '=~', '!~'):
            op = toks[i]; i += 1; right = val(toks[i]); i += 1
            if op == '==': return left == right
            if op == '!=': return left != right
            pat = right[1].replace('\\/', '/')
            m = left is not None and re.search(pat, left) is not None
            return m if op == '=~' else not m
        return left not in (None, '')
    def andexp():
        nonlocal i
        r = primary()
        while i < len(toks) and toks[i] == '&&':
            i += 1; r2 = primary(); r = r and r2
        return r
    def orexp():
        nonlocal i
        r = andexp()
        while i < len(toks) and toks[i] == '||':
            i += 1; r2 = andexp(); r = r or r2
        return r
    r = orexp(); assert i == len(toks), expr
    return bool(r)

def match_rules(rules, env):
    for r in rules:
        if 'if' in r and not ev(r['if'], env):
            continue
        # `changes`/`exists` treated as true (conservative: job appears)
        return r
    return None

def expand_vars(s, env):
    if not isinstance(s, str): return s
    for _ in range(5):
        s2 = re.sub(r'\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?', lambda m: env.get(m.group(1), ''), s)
        if s2 == s: break
        s = s2
    return s

def pipeline(cfg, env):
    gvars = {k: (v['value'] if isinstance(v, dict) else v) for k, v in cfg.get('variables', {}).items()}
    # rules:if sees the global YAML variables too (P5_FAST_MR); pipeline
    # variables (env) override them.
    env = {**gvars, **env}
    wf = match_rules(cfg['workflow']['rules'], env)
    if wf is None or wf.get('when') == 'never':
        return None
    wvars = wf.get('variables', {})
    jobs = {}
    for name, job in cfg.items():
        if name.startswith('.') or not isinstance(job, dict) or name in ('workflow', 'variables', 'include', 'stages', 'default'):
            continue
        if 'script' not in job:
            continue
        rules = job.get('rules')
        r = match_rules(rules, env) if rules is not None else {}
        if r is None or r.get('when') == 'never':
            continue
        eff = {k: v for k, v in job.items() if k != 'rules'}
        for k in ('if', 'when', 'interruptible', 'allow_failure', 'needs', 'changes'):
            if k in r: eff['rule_' + k] = r[k]
        # variable precedence: pipeline vars (env user) > rules vars > job vars > workflow vars > global vars
        jv = dict(gvars); jv.update(wvars); jv.update(job.get('variables', {}) or {}); jv.update(r.get('variables', {}) or {})
        for k in env.get('_user', []):
            jv[k] = env[k]
        eff['effective_variables'] = jv
        venv = dict(env); venv.update({k: expand_vars(v, {**env, **jv}) for k, v in jv.items()})
        if 'resource_group' in job:
            eff['resource_group_expanded'] = expand_vars(job['resource_group'], venv)
        if 'tags' in job:
            eff['tags_expanded'] = [expand_vars(t, venv) for t in job['tags']]
        if 'cache' in job:
            eff['cache_policies'] = [expand_vars(c.get('policy', 'pull-push'), venv) for c in job['cache']]
        jobs[name] = eff
    # drop jobs whose mandatory needs are missing -> GitLab errors
    errors = []
    for n, j in jobs.items():
        for nd in j.get('needs', []) or []:
            nn = nd['job'] if isinstance(nd, dict) else nd
            opt = isinstance(nd, dict) and nd.get('optional')
            if nn not in jobs and not opt:
                errors.append(f'{n} needs {nn}')
    return {'jobs': jobs, 'errors': errors, 'workflow': wf}

ALL = ['rust-fmt', 'ip-boundary-skill-installed', 'scope-register', 'v4-provenance', 'v4-runtime',
       'change-class', 'public-api', 'main-smoke', 'lint', 'mr-quick', 'mr-lint']
SOURCES = ['push', 'merge_request_event', 'schedule', 'api', 'web', 'trigger', 'pipeline']
BRANCHES = ['main', 'integration/x', 'feature/x', 'weave-bisect/x', 'weave-bisect', 'xweave-bisect/x', 'TAG']
FLAGS = [None, '1', '0', 'true']
JOBSETS = [None, 'v4-runtime', 'rust-fmt,public-api', 'rust-fmt, public-api', 'v4-runtime ,rust-fmt',
           'lint,mr-quick,mr-lint', ','.join(ALL), 'no-such']

def scenarios():
    for s, b, f, js in itertools.product(SOURCES, BRANCHES, FLAGS, JOBSETS):
        if s == 'merge_request_event' and b == 'TAG':
            continue
        yield s, b, f, js

def base_env(source, branch, flag=None, jobs=None, extra=None):
    e = {'CI_PIPELINE_SOURCE': source, 'CI_DEFAULT_BRANCH': 'main', 'CI_PROJECT_PATH': 'root/lean-ctx',
         'CI_PROJECT_ID': '5', 'CI_PROJECT_PATH_SLUG': 'root-lean-ctx', 'CI_CONCURRENT_ID': '0',
         'CI_BUILDS_DIR': '/builds', '_user': []}
    if source == 'merge_request_event':
        e['CI_MERGE_REQUEST_IID'] = '520'; e['CI_MERGE_REQUEST_EVENT_TYPE'] = 'detached'
        e['CI_MERGE_REQUEST_SOURCE_BRANCH_NAME'] = branch; e['CI_COMMIT_REF_SLUG'] = 'mr'
    elif branch == 'TAG':
        e['CI_COMMIT_TAG'] = 'v1'; e['CI_COMMIT_REF_SLUG'] = 'v1'
    else:
        e['CI_COMMIT_BRANCH'] = branch; e['CI_COMMIT_REF_SLUG'] = re.sub(r'[^a-z0-9]', '-', branch.lower())
    if flag is not None:
        e['WEAVE_SCHEDULE_BISECT'] = flag; e['_user'].append('WEAVE_SCHEDULE_BISECT')
    if jobs is not None:
        e['WEAVE_BISECT_JOBS'] = jobs; e['_user'].append('WEAVE_BISECT_JOBS')
    for k, v in (extra or {}).items():
        e[k] = v; e['_user'].append(k)
    return e

if __name__ == '__main__':
    B = expand(sys.argv[1]); A = expand(sys.argv[2])
    diffs = []; bisect_rows = []
    for s, b, f, js in scenarios():
        env = base_env(s, b, f, js)
        pb = pipeline(B, env); pa = pipeline(A, env)
        is_bisect = (s == 'api' and f == '1' and b.startswith('weave-bisect/'))
        def norm(p):
            if p is None: return None
            jj={}
            for n,j in p['jobs'].items():
                j={k:v for k,v in j.items() if k!='resource_group'}
                for vk in ('variables','effective_variables'):
                    if vk in j: j[vk]={a:b for a,b in j[vk].items() if a!='V4_RESOURCE_GROUP'}
                jj[n]=j
            return {'jobs': jj, 'errors': p['errors'], 'wf': p['workflow']}
        same = json.dumps(norm(pb), sort_keys=True, default=str) == json.dumps(norm(pa), sort_keys=True, default=str)
        if not same and not is_bisect:
            diffs.append((s, b, f, js, pb and sorted(pb['jobs']), pa and sorted(pa['jobs'])))
            if pb and pa:
                for n in set(pb['jobs']) | set(pa['jobs']):
                    x = json.dumps(pb['jobs'].get(n), sort_keys=True, default=str); y = json.dumps(pa['jobs'].get(n), sort_keys=True, default=str)
                    if x != y:
                        diffs.append(('  job', n, pb['jobs'].get(n, {}).get('resource_group_expanded'), pa['jobs'].get(n, {}).get('resource_group_expanded')))
        if is_bisect and b == 'weave-bisect/x' and s == 'api':
            bisect_rows.append((js, None if pa is None else sorted(pa['jobs']), None if pa is None else pa['errors'],
                                None if pa is None else {n: (j.get('resource_group_expanded'), j.get('tags_expanded'), j.get('cache_policies'), j.get('rule_interruptible')) for n, j in pa['jobs'].items()}))
    print('NON-BISECT DIFFS:', len(diffs))
    for d in diffs[:60]: print(d)
    print('\nBISECT (api, weave-bisect/x, flag=1):')
    seen = set()
    for r in bisect_rows:
        k = json.dumps(r, default=str)
        if k in seen: continue
        seen.add(k); print(json.dumps(r, default=str, indent=None))
