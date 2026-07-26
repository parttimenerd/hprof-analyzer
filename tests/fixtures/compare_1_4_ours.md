## Cross-Dump Growth

_How the reachable heap grew across a time series of dumps of the same application (first = baseline, last = current)._

### Reports

- `r1` = dump_1_mnemonics.hprof
- `r2` = dump_4_philosophers.hprof

**Verdict:** Heap grew 3.3% (+377.4 KB shallow); largest driver `scala.concurrent.stm.ccstm.InTxnImpl` (+3.7 MB retained). Gross retained churn: +15.7 MB grown / −14.3 MB reclaimed across steps. 3 new suspects.

### Headline Totals

- **Δ Objects (r1→rN):** −98,418
- **Δ Shallow heap (r1→rN):** +377.4 KB
- **Gross growth (classes that grew, r1→rN):** +15.7 MB
- **Gross reclaimed (classes that shrank, r1→rN):** −14.3 MB
- **Gross Retained churn (growth + reclaimed, r1→rN):** +29.9 MB
- **Net Δ Retained (all classes, r1→rN):** +1.4 MB

### Growth Leaders (by peak−baseline retained)

| Class                                                                 |       r1 |       r2 |  Δ(r1→rN) |
| --------------------------------------------------------------------- | -------: | -------: | --------: |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                |      0 B |   3.7 MB |   +3.7 MB |
| `long[]`                                                              |  14.2 KB |   1.1 MB |   +1.1 MB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread` |      0 B |   1.0 MB |   +1.0 MB |
| `scala.concurrent.stm.ccstm.Handle[]`                                 |      0 B |   1.0 MB |   +1.0 MB |
| `java.util.zip.ZipFile$Source`                                        | 449.7 KB |   1.3 MB | +873.1 KB |
| `int[]`                                                               | 106.7 KB | 875.9 KB | +769.2 KB |
| `java.util.jar.Manifest`                                              | 592.1 KB |   1.1 MB | +520.1 KB |
| `java.util.jar.JarFile`                                               | 602.9 KB |   1.1 MB | +515.4 KB |
| `java.lang.ref.SoftReference`                                         | 618.5 KB |   1.1 MB | +512.5 KB |
| `scala.concurrent.stm.skel.CallbackList`                              |      0 B | 417.2 KB | +417.2 KB |
| `scala.Function1[]`                                                   |      0 B | 399.1 KB | +399.1 KB |
| `java.util.concurrent.ConcurrentHashMap`                              | 249.6 KB | 611.6 KB | +362.0 KB |
| `java.util.LinkedHashMap`                                             | 197.4 KB | 507.4 KB | +310.0 KB |
| `java.lang.Thread`                                                    |   5.1 KB | 309.2 KB | +304.1 KB |
| `java.lang.Object[]`                                                  |   3.4 MB |   3.7 MB | +298.3 KB |
| `java.lang.Class`                                                     |   3.3 MB |   3.6 MB | +298.2 KB |
| `java.util.concurrent.ConcurrentHashMap$Node[]`                       | 361.0 KB | 604.7 KB | +243.7 KB |
| `java.util.jar.Attributes`                                            | 240.2 KB | 448.0 KB | +207.9 KB |
| `java.time.zone.ZoneRulesProvider`                                    |     80 B | 198.4 KB | +198.3 KB |
| `java.util.HashMap`                                                   |   1.4 MB |   1.5 MB | +179.2 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$CameraThread`      |      0 B | 157.5 KB | +157.5 KB |
| `org.renaissance.core.ModuleLoader`                                   |   3.5 KB | 149.8 KB | +146.4 KB |
| `java.util.LinkedHashSet`                                             |    648 B | 144.6 KB | +143.9 KB |
| `sun.util.calendar.ZoneInfoFile`                                      |  14.1 KB | 145.4 KB | +131.3 KB |
| `java.time.zone.TzdbZoneRulesProvider`                                |     88 B | 118.2 KB | +118.1 KB |

### New Classes

| Class                                                                 |  r1 |       r2 |  Δ(r1→rN) |
| --------------------------------------------------------------------- | --: | -------: | --------: |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                | 0 B |   3.7 MB |   +3.7 MB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread` | 0 B |   1.0 MB |   +1.0 MB |
| `scala.concurrent.stm.ccstm.Handle[]`                                 | 0 B |   1.0 MB |   +1.0 MB |
| `scala.concurrent.stm.skel.CallbackList`                              | 0 B | 417.2 KB | +417.2 KB |
| `scala.Function1[]`                                                   | 0 B | 399.1 KB | +399.1 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$CameraThread`      | 0 B | 157.5 KB | +157.5 KB |
| `scala.concurrent.stm.skel.SimpleRandom$`                             | 0 B |  64.5 KB |  +64.5 KB |
| `scala.Console$`                                                      | 0 B |  24.6 KB |  +24.6 KB |
| `scala.util.DynamicVariable`                                          | 0 B |  24.5 KB |  +24.5 KB |
| `java.io.BufferedReader`                                              | 0 B |  24.4 KB |  +24.4 KB |
| `java.util.concurrent.atomic.AtomicReferenceArray`                    | 0 B |  13.2 KB |  +13.2 KB |
| `scala.concurrent.stm.ccstm.TxnLevelImpl`                             | 0 B |  13.1 KB |  +13.1 KB |
| `scala.collection.GenIterable`                                        | 0 B |  11.2 KB |  +11.2 KB |
| `scala.collection.GenTraversable`                                     | 0 B |  10.5 KB |  +10.5 KB |
| `scala.concurrent.stm.ccstm.WakeupManager`                            | 0 B |   8.7 KB |   +8.7 KB |
| `java.io.InputStreamReader`                                           | 0 B |   8.3 KB |   +8.3 KB |
| `scala.concurrent.stm.ccstm.TxnSlotManager`                           | 0 B |   8.3 KB |   +8.3 KB |
| `sun.nio.cs.StreamDecoder`                                            | 0 B |   8.3 KB |   +8.3 KB |
| `scala.concurrent.stm.ccstm.RetrySet`                                 | 0 B |   5.4 KB |   +5.4 KB |
| `scala.collection.immutable.StringLike`                               | 0 B |   4.3 KB |   +4.3 KB |
| `java.util.concurrent.atomic.AtomicLongArray`                         | 0 B |   4.3 KB |   +4.3 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$GenericRef`                     | 0 B |   4.0 KB |   +4.0 KB |
| `scala.collection.mutable.ResizableArray`                             | 0 B |   3.3 KB |   +3.3 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$IntRef`                         | 0 B |   3.0 KB |   +3.0 KB |
| `scala.collection.MapLike`                                            | 0 B |   2.5 KB |   +2.5 KB |

### Removed Classes

| Class                                                                             |     r1 |  r2 | Δ(r1→rN) |
| --------------------------------------------------------------------------------- | -----: | --: | -------: |
| `org.renaissance.jdk.streams.MnemonicsCoderWithStream`                            | 1.6 KB | 0 B |  −1.6 KB |
| `scopt.OptionDef$$Lambda+0x00007ff1d40dcd20`                                      |  488 B | 0 B |   −488 B |
| `org.renaissance.jdk.streams.MnemonicsCoderWithStream$$Lambda+0x00007ff1d4127800` |  384 B | 0 B |   −384 B |
| `org.renaissance.jdk.streams.MnemonicsCoderWithStream$$Lambda+0x00007ff1d4126b18` |  256 B | 0 B |   −256 B |
| `java.util.stream.LongPipeline`                                                   |  224 B | 0 B |   −224 B |
| `org.renaissance.jdk.streams.Mnemonics`                                           |  200 B | 0 B |   −200 B |
| `java.lang.invoke.BoundMethodHandle$Species_LLLLL`                                |  144 B | 0 B |   −144 B |
| `org.renaissance.harness.Config$$Lambda+0x00007ff1d40fc678`                       |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40dd6a0`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40de018`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40de990`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40def38`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40df4e0`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e0000`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e05a8`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e0b50`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e10f8`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e16a0`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e1c48`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e21f0`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e2798`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e2d40`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e3890`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e3e38`         |  112 B | 0 B |   −112 B |
| `org.renaissance.harness.ConfigParser$$anon$1$$Lambda+0x00007ff1d40e43e0`         |  112 B | 0 B |   −112 B |

### New / Grown Leak Suspects

| Suspect                                |  r1 |     r2 | Δ(r1→rN) | New? |
| -------------------------------------- | --: | -----: | -------: | ---- |
| `scala.concurrent.stm.ccstm.InTxnImpl` | 0 B | 2.7 MB |  +2.7 MB | yes  |
| `scala.runtime.LazyVals$`              | 0 B | 2.5 MB |  +2.5 MB | yes  |
| `java.util.zip.ZipFile$Source`         | 0 B | 1.3 MB |  +1.3 MB | yes  |

### Shrunk Leak Suspects

No leak suspect shrank in the current dump.

### Disappeared Leak Suspects

| Suspect                   |     r1 |  r2 | Δ(r1→rN) |
| ------------------------- | -----: | --: | -------: |
| `byte[]`                  | 3.0 MB | 0 B |  −3.0 MB |
| `java.net.URLClassLoader` | 2.6 MB | 0 B |  −2.6 MB |
| `java.util.HashMap$Node`  | 1.9 MB | 0 B |  −1.9 MB |

