import os, re

base = '/Users/i560383_1/code/experiments/hprof-analyzer'
pattern = re.compile(r'gc.?roots', re.IGNORECASE)
for dirpath, dirs, files in os.walk(base):
    dirs[:] = [d for d in dirs if d not in ('target', '.git', '.claude')]
    for fname in files:
        if fname.endswith('.rs'):
            fpath = os.path.join(dirpath, fname)
            try:
                with open(fpath) as f:
                    for i, line in enumerate(f, 1):
                        if pattern.search(line):
                            print(f'{fpath}:{i}: {line.rstrip()}')
            except Exception:
                pass
