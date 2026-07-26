## Cross-Dump Growth

_How the reachable heap grew across a time series of dumps of the same application (first = baseline, last = current)._

### Reports

- `r1` = dump_4_philosophers.hprof
- `r2` = dump_2_scala-doku.hprof

**Verdict:** Heap grew 156.4% (+18.2 MB shallow); largest driver `cafesat.sat.Literal` (+4.8 MB retained). Gross retained churn: +25.5 MB grown / −4.0 MB reclaimed across steps. 2 new suspects.

### Headline Totals

- **Δ Objects (r1→rN):** +716,209
- **Δ Shallow heap (r1→rN):** +18.2 MB
- **Gross growth (classes that grew, r1→rN):** +25.5 MB
- **Gross reclaimed (classes that shrank, r1→rN):** −4.0 MB
- **Gross Retained churn (growth + reclaimed, r1→rN):** +29.5 MB
- **Net Δ Retained (all classes, r1→rN):** +21.5 MB

### Growth Leaders (by peak−baseline retained)

| Class                                             |       r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------- | -------: | -------: | --------: |
| `cafesat.sat.Literal`                             |      0 B |   4.8 MB |   +4.8 MB |
| `cafesat.sat.Vector`                              |      0 B |   4.2 MB |   +4.2 MB |
| `cafesat.sat.Solver$Clause[]`                     |      0 B |   3.4 MB |   +3.4 MB |
| `scala.collection.immutable.Set$Set2`             |      0 B |   2.7 MB |   +2.7 MB |
| `cafesat.sat.Solver$Clause`                       |      0 B |   2.0 MB |   +2.0 MB |
| `int[]`                                           | 875.9 KB |   2.6 MB |   +1.8 MB |
| `scala.collection.immutable.BitmapIndexedSetNode` |      0 B |   1.5 MB |   +1.5 MB |
| `java.lang.Object[]`                              |   3.4 MB |   4.5 MB |   +1.1 MB |
| `cafesat.asts.core.Trees$ConnectiveApplication`   |      0 B |   1.0 MB |   +1.0 MB |
| `cafesat.asts.core.Trees$ConnectiveSymbol`        |      0 B | 825.9 KB | +825.9 KB |
| `scala.collection.immutable.Set$Set3`             |      0 B | 546.7 KB | +546.7 KB |
| `cafesat.asts.core.Trees$PredicateApplication`    |      0 B | 463.2 KB | +463.2 KB |
| `cafesat.api.Formulas$Formula`                    |      0 B | 347.9 KB | +347.9 KB |
| `java.lang.Integer`                               |   5.1 KB | 153.4 KB | +148.2 KB |
| `cafesat.sat.Vector[]`                            |      0 B | 143.0 KB | +143.0 KB |
| `scala.collection.immutable.HashSet`              |      0 B |  82.9 KB |  +82.9 KB |
| `cafesat.sat.Solver`                              |      0 B |  72.4 KB |  +72.4 KB |
| `java.util.concurrent.ConcurrentHashMap`          |  74.4 KB | 135.8 KB |  +61.4 KB |
| `cafesat.api.Formulas$Formula[]`                  |      0 B |  35.1 KB |  +35.1 KB |
| `java.lang.Thread`                                |   3.4 KB |  38.4 KB |  +35.1 KB |
| `java.lang.Object[][]`                            |     16 B |  28.8 KB |  +28.8 KB |
| `org.renaissance.core.BenchmarkDescriptor`        |    752 B |  28.2 KB |  +27.5 KB |
| `scala.collection.immutable.BitmapIndexedMapNode` |      0 B |  24.9 KB |  +24.9 KB |
| `java.lang.Module`                                |   6.1 KB |  25.7 KB |  +19.6 KB |
| `java.util.LinkedHashMap`                         | 344.9 KB | 364.2 KB |  +19.3 KB |

### New Classes

| Class                                             |  r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------- | --: | -------: | --------: |
| `cafesat.sat.Literal`                             | 0 B |   4.8 MB |   +4.8 MB |
| `cafesat.sat.Vector`                              | 0 B |   4.2 MB |   +4.2 MB |
| `cafesat.sat.Solver$Clause[]`                     | 0 B |   3.4 MB |   +3.4 MB |
| `scala.collection.immutable.Set$Set2`             | 0 B |   2.7 MB |   +2.7 MB |
| `cafesat.sat.Solver$Clause`                       | 0 B |   2.0 MB |   +2.0 MB |
| `scala.collection.immutable.BitmapIndexedSetNode` | 0 B |   1.5 MB |   +1.5 MB |
| `cafesat.asts.core.Trees$ConnectiveApplication`   | 0 B |   1.0 MB |   +1.0 MB |
| `cafesat.asts.core.Trees$ConnectiveSymbol`        | 0 B | 825.9 KB | +825.9 KB |
| `scala.collection.immutable.Set$Set3`             | 0 B | 546.7 KB | +546.7 KB |
| `cafesat.asts.core.Trees$PredicateApplication`    | 0 B | 463.2 KB | +463.2 KB |
| `cafesat.api.Formulas$Formula`                    | 0 B | 347.9 KB | +347.9 KB |
| `cafesat.sat.Vector[]`                            | 0 B | 143.0 KB | +143.0 KB |
| `scala.collection.immutable.HashSet`              | 0 B |  82.9 KB |  +82.9 KB |
| `cafesat.sat.Solver`                              | 0 B |  72.4 KB |  +72.4 KB |
| `cafesat.api.Formulas$Formula[]`                  | 0 B |  35.1 KB |  +35.1 KB |
| `scala.collection.immutable.BitmapIndexedMapNode` | 0 B |  24.9 KB |  +24.9 KB |
| `cafesat.asts.core.Trees$PredicateSymbol`         | 0 B |  17.1 KB |  +17.1 KB |
| `scala.collection.immutable.Vector3`              | 0 B |  12.7 KB |  +12.7 KB |
| `cafesat.api.Formulas$PropVar`                    | 0 B |  11.4 KB |  +11.4 KB |
| `cafesat.api.Formulas$PropVar[]`                  | 0 B |   5.7 KB |   +5.7 KB |
| `scala.Option[][]`                                | 0 B |   3.4 KB |   +3.4 KB |
| `org.renaissance.scala.sat.ScalaDoku`             | 0 B |   1.9 KB |   +1.9 KB |
| `scala.collection.immutable.Vector2`              | 0 B |   1.7 KB |   +1.7 KB |
| `cafesat.api.Formulas$PropVar[][]`                | 0 B |   1.1 KB |   +1.1 KB |
| `cafesat.asts.fol.Manip$`                         | 0 B |    784 B |    +784 B |

### Removed Classes

| Class                                                                                            |       r1 |  r2 |  Δ(r1→rN) |
| ------------------------------------------------------------------------------------------------ | -------: | --: | --------: |
| `scala.concurrent.stm.ccstm.Handle[]`                                                            |   1.0 MB | 0 B |   −1.0 MB |
| `scala.Function1[]`                                                                              | 399.1 KB | 0 B | −399.1 KB |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                                           | 324.7 KB | 0 B | −324.7 KB |
| `scala.concurrent.stm.skel.CallbackList`                                                         |  34.1 KB | 0 B |  −34.1 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread`                            |  28.9 KB | 0 B |  −28.9 KB |
| `java.util.concurrent.atomic.AtomicReferenceArray`                                               |  12.8 KB | 0 B |  −12.8 KB |
| `scala.concurrent.stm.ccstm.TxnLevelImpl`                                                        |  11.7 KB | 0 B |  −11.7 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$GenericRef`                                                |   3.2 KB | 0 B |   −3.2 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$IntRef`                                                    |   3.0 KB | 0 B |   −3.0 KB |
| `scala.concurrent.stm.ccstm.RetrySet`                                                            |   2.4 KB | 0 B |   −2.4 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$Fork`                                         |   2.0 KB | 0 B |   −2.0 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01b5210` |   2.0 KB | 0 B |   −2.0 KB |
| `scala.sys.SystemProperties`                                                                     |   1.8 KB | 0 B |   −1.8 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01b7be0` |   1.6 KB | 0 B |   −1.6 KB |
| `scala.collection.GenTraversable`                                                                |   1.4 KB | 0 B |   −1.4 KB |
| `scala.concurrent.stm.ccstm.WakeupManager$EventImpl`                                             |    984 B | 0 B |    −984 B |
| `scala.collection.GenIterable`                                                                   |    768 B | 0 B |    −768 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$Fork[]`                                       |    720 B | 0 B |    −720 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread$$Lambda+0x00007e41c01deae0` |    568 B | 0 B |    −568 B |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread[]`                          |    528 B | 0 B |    −528 B |
| `scala.collection.TraversableLike`                                                               |    512 B | 0 B |    −512 B |
| `scopt.OptionDef$$Lambda+0x00007e41c00dcd20`                                                     |    488 B | 0 B |    −488 B |
| `scala.collection.GenMap`                                                                        |    416 B | 0 B |    −416 B |
| `scala.collection.GenSeqLike`                                                                    |    384 B | 0 B |    −384 B |
| `scala.collection.GenMapLike`                                                                    |    344 B | 0 B |    −344 B |

### New / Grown Leak Suspects

| Suspect                                   |  r1 |     r2 | Δ(r1→rN) | New? |
| ----------------------------------------- | --: | -----: | -------: | ---- |
| `cafesat.sat.Vector`                      | 0 B | 4.2 MB |  +4.2 MB | yes  |
| `scala.collection.immutable.$colon$colon` | 0 B | 3.4 MB |  +3.4 MB | yes  |

### Shrunk Leak Suspects

No leak suspect shrank in the current dump.

### Disappeared Leak Suspects

| Suspect                   |     r1 |  r2 | Δ(r1→rN) |
| ------------------------- | -----: | --: | -------: |
| `java.net.URLClassLoader` | 2.6 MB | 0 B |  −2.6 MB |
| `byte[]`                  | 1.5 MB | 0 B |  −1.5 MB |

