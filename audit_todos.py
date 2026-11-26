import os

src_root = 'src'
patterns = ['TODO', 'FIXME', 'HACK', 'XXX', 'todo!', 'unimplemented!', 'unreachable!']
results = []
for root, dirs, files in os.walk(src_root):
    for f in files:
        if not f.endswith('.rs'):
            continue
        path = os.path.join(root, f)
        with open(path) as fh:
            for lineno, line in enumerate(fh, 1):
                for p in patterns:
                    if p in line:
                        results.append((path, lineno, line.rstrip()))
                        break

for path, lineno, line in results:
    print(f'{path}:{lineno}: {line.strip()}')
print(f'\n--- TOTAL: {len(results)} findings ---')
