TB = 1024**4
GB = 1024**3

n = TB - 1
gb = n / GB
print(f"format_bytes(TB-1): n={n}")
print(f"  gb = {gb}")
print(f"  round(gb * 100) = {round(gb * 100.0)}")
print(f"  threshold = {1024.0 * 100.0}")
print(f"  n < TB? {n < TB}")
print(f"  round(gb*100) < 102400? {round(gb * 100.0) < 1024.0 * 100.0}")
tb = n / TB
print(f"  Falls to TB branch: {tb:.2f} TB  <-- WRONG, should be GB")
print()
MB = 1024**2
KB = 1024
n2 = MB - 1
kb = n2 / KB
print(f"format_bytes(MB-1): n={n2}")
print(f"  kb = {kb}")
print(f"  round(kb*10) = {round(kb * 10.0)}")
print(f"  n < MB? {n2 < MB}")
print(f"  round(kb*10) < 10240? {round(kb * 10.0) < 1024.0 * 10.0}")
mb = n2 / MB
print(f"  Falls to MB branch: {mb:.1f} MB  <-- WRONG, should be KB")
