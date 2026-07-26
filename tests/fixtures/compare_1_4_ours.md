## Cross-Dump Growth

_How the reachable heap grew across a time series of dumps of the same application (first = baseline, last = current)._

### Reports

- `r1` = dump_1_mnemonics.hprof
- `r2` = dump_4_philosophers.hprof

**Verdict:** Heap grew 3.3% (+377.4 KB shallow); largest driver `long[]` (+1.1 MB retained). Gross retained churn: +4.2 MB grown / −17.4 MB reclaimed across steps.

### Headline Totals

- **Δ Objects (r1→rN):** −98,418
- **Δ Shallow heap (r1→rN):** +377.4 KB
- **Gross growth (classes that grew, r1→rN):** +4.2 MB
- **Gross reclaimed (classes that shrank, r1→rN):** −17.4 MB
- **Gross Retained churn (growth + reclaimed, r1→rN):** +21.6 MB
- **Net Δ Retained (all classes, r1→rN):** −13.1 MB

### Growth Leaders (by peak−baseline retained)

| Class                                                                                            |       r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------------------------------------------------------ | -------: | -------: | --------: |
| `long[]`                                                                                         |  14.2 KB |   1.1 MB |   +1.1 MB |
| `scala.concurrent.stm.ccstm.Handle[]`                                                            |      0 B |   1.0 MB |   +1.0 MB |
| `int[]`                                                                                          | 106.7 KB | 875.9 KB | +769.2 KB |
| `scala.Function1[]`                                                                              |      0 B | 399.1 KB | +399.1 KB |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                                           |      0 B | 324.7 KB | +324.7 KB |
| `java.net.URLClassLoader`                                                                        |   2.6 MB |   2.8 MB | +234.7 KB |
| `java.util.LinkedHashMap`                                                                        | 197.4 KB | 344.9 KB | +147.5 KB |
| `java.util.zip.ZipFile$Source`                                                                   | 449.7 KB | 558.0 KB | +108.3 KB |
| `scala.concurrent.stm.skel.CallbackList`                                                         |      0 B |  34.1 KB |  +34.1 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread`                            |      0 B |  28.9 KB |  +28.9 KB |
| `java.lang.Object`                                                                               |   2.0 MB |   2.0 MB |  +20.3 KB |
| `char[]`                                                                                         |  55.7 KB |  71.8 KB |  +16.0 KB |
| `java.lang.ThreadLocal$ThreadLocalMap$Entry[]`                                                   |    136 B |  14.1 KB |  +14.0 KB |
| `java.util.concurrent.atomic.AtomicReferenceArray`                                               |      0 B |  12.8 KB |  +12.8 KB |
| `scala.concurrent.stm.ccstm.TxnLevelImpl`                                                        |      0 B |  11.7 KB |  +11.7 KB |
| `java.lang.ThreadLocal$ThreadLocalMap$Entry`                                                     |    152 B |  10.4 KB |  +10.2 KB |
| `java.lang.Thread$FieldHolder`                                                                   |   1.2 KB |   6.3 KB |   +5.0 KB |
| `java.security.ProtectionDomain[]`                                                               |    224 B |   5.2 KB |   +5.0 KB |
| `java.security.AccessControlContext`                                                             |   1.5 KB |   6.4 KB |   +4.9 KB |
| `java.lang.ThreadLocal$ThreadLocalMap`                                                           |     32 B |   3.3 KB |   +3.2 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$GenericRef`                                                |      0 B |   3.2 KB |   +3.2 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$IntRef`                                                    |      0 B |   3.0 KB |   +3.0 KB |
| `scala.concurrent.stm.ccstm.RetrySet`                                                            |      0 B |   2.4 KB |   +2.4 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$Fork`                                         |      0 B |   2.0 KB |   +2.0 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01b5210` |      0 B |   2.0 KB |   +2.0 KB |

### New Classes

| Class                                                                                            |  r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------------------------------------------------------ | --: | -------: | --------: |
| `scala.concurrent.stm.ccstm.Handle[]`                                                            | 0 B |   1.0 MB |   +1.0 MB |
| `scala.Function1[]`                                                                              | 0 B | 399.1 KB | +399.1 KB |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                                           | 0 B | 324.7 KB | +324.7 KB |
| `scala.concurrent.stm.skel.CallbackList`                                                         | 0 B |  34.1 KB |  +34.1 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread`                            | 0 B |  28.9 KB |  +28.9 KB |
| `java.util.concurrent.atomic.AtomicReferenceArray`                                               | 0 B |  12.8 KB |  +12.8 KB |
| `scala.concurrent.stm.ccstm.TxnLevelImpl`                                                        | 0 B |  11.7 KB |  +11.7 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$GenericRef`                                                | 0 B |   3.2 KB |   +3.2 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$IntRef`                                                    | 0 B |   3.0 KB |   +3.0 KB |
| `scala.concurrent.stm.ccstm.RetrySet`                                                            | 0 B |   2.4 KB |   +2.4 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$Fork`                                         | 0 B |   2.0 KB |   +2.0 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01b5210` | 0 B |   2.0 KB |   +2.0 KB |
| `scala.sys.SystemProperties`                                                                     | 0 B |   1.8 KB |   +1.8 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01b7be0` | 0 B |   1.6 KB |   +1.6 KB |
| `scala.collection.GenTraversable`                                                                | 0 B |   1.4 KB |   +1.4 KB |
| `scala.concurrent.stm.ccstm.WakeupManager$EventImpl`                                             | 0 B |    984 B |    +984 B |
| `scala.collection.GenIterable`                                                                   | 0 B |    768 B |    +768 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$Fork[]`                                       | 0 B |    720 B |    +720 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01deae0` | 0 B |    568 B |    +568 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread[]`                          | 0 B |    528 B |    +528 B |
| `scala.collection.TraversableLike`                                                               | 0 B |    512 B |    +512 B |
| `scopt.OptionDef$$Lambda+0x00007e41c00dcd20`                                                     | 0 B |    488 B |    +488 B |
| `scala.collection.GenMap`                                                                        | 0 B |    416 B |    +416 B |
| `scala.collection.GenSeqLike`                                                                    | 0 B |    384 B |    +384 B |
| `scala.collection.GenMapLike`                                                                    | 0 B |    344 B |    +344 B |

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

No leak suspect is new or grew in the current dump.

### Shrunk Leak Suspects

| Suspect                   |     r1 |     r2 | Δ(r1→rN) |
| ------------------------- | -----: | -----: | -------: |
| `byte[]`                  | 3.0 MB | 1.5 MB |  −1.5 MB |
| `java.net.URLClassLoader` | 2.6 MB | 2.6 MB |   −544 B |

### Disappeared Leak Suspects

| Suspect                  |     r1 |  r2 | Δ(r1→rN) |
| ------------------------ | -----: | --: | -------: |
| `java.util.HashMap$Node` | 1.9 MB | 0 B |  −1.9 MB |

