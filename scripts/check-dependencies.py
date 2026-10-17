"""Read-only OSV scan of locked crates, distinguishing the Windows build graph.

Usage: python scripts/check-dependencies.py [output.json]
Requires cargo on PATH. Queries contain only public crate names and versions.
"""
import concurrent.futures
import json
import pathlib
import re
import subprocess
import sys
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]


def main():
    # Cargo itself parses the lockfile. Metadata includes all platform packages,
    # while the tree below determines which are reachable in this Windows build.
    metadata = json.loads(subprocess.check_output([
        'cargo', 'metadata', '--format-version', '1', '--locked', '--offline'
    ], cwd=ROOT, text=True, encoding='utf-8'))
    packages = sorted({(p['name'], p['version']) for p in metadata['packages']
                       if (p.get('source') or '').startswith('registry+')})
    graph = subprocess.check_output([
        'cargo', 'tree', '--locked', '--offline', '--target',
        'x86_64-pc-windows-msvc', '--edges', 'normal,build',
        '--prefix', 'none', '--format', '{p}'
    ], cwd=ROOT, text=True, encoding='utf-8')
    active = {tuple(m.groups()) for line in graph.splitlines()
              if (m := re.match(r'^([\w-]+) v([^\s]+)', line))}

    def query(chunk):
        body = {'queries': [{'package': {'name': n, 'ecosystem': 'crates.io'},
                             'version': v} for n, v in chunk]}
        request = urllib.request.Request('https://api.osv.dev/v1/querybatch',
                                         json.dumps(body).encode(),
                                         {'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=40) as response:
            data = json.load(response)
        results = data.get('results')
        if not isinstance(results, list) or len(results) != len(chunk):
            raise RuntimeError('OSV returned an incomplete batch')
        return [{'name': n, 'version': v, 'in_windows_build': (n, v) in active,
                 'advisories': [item['id'] for item in result.get('vulns', [])]}
                for (n, v), result in zip(chunk, results)]

    chunks = [packages[i:i+100] for i in range(0, len(packages), 100)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = [row for result in pool.map(query, chunks) for row in result]
    report = {'scanned_packages': len(rows), 'windows_graph_packages': len(active),
              'findings': [row for row in rows if row['advisories']]}
    target = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/audit-remediation-20261015/dependency-scan.json'
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report, ensure_ascii=True))


if __name__ == '__main__':
    main()
