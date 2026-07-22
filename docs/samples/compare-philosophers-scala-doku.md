## Cross-Dump Growth

_How the reachable heap grew across a time series of dumps of the same application (first = baseline, last = current)._

### Reports

- `r1` = dump_4_philosophers.hprof
- `r2` = dump_2_scala-doku.hprof

**Verdict:** Heap grew 156.4% (+18.2 MB shallow); largest driver `java.lang.Thread` (+22.6 MB retained). 2 new suspects.

### Headline Totals

- **Δ Objects (r1→rN):** +716,209
- **Δ Shallow heap (r1→rN):** +18.2 MB
- **Net Δ Retained (all classes, r1→rN):** +76.1 MB
- **Gross Retained churn (all classes, per-step):** +85.6 MB grown / −9.6 MB reclaimed

### Growth Leaders (by Δ retained)

| Class                                             |       r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------- | -------: | -------: | --------: |
| `java.lang.Thread`                                | 309.2 KB |  22.9 MB |  +22.6 MB |
| `scala.collection.immutable.BitmapIndexedSetNode` |      0 B |   8.4 MB |   +8.4 MB |
| `scala.collection.immutable.HashSet`              |      0 B |   8.0 MB |   +8.0 MB |
| `java.lang.Object[]`                              |   3.7 MB |  11.4 MB |   +7.7 MB |
| `cafesat.sat.Literal`                             |      0 B |   4.8 MB |   +4.8 MB |
| `cafesat.sat.Solver`                              |      0 B |   4.6 MB |   +4.6 MB |
| `scala.collection.immutable.Set$Set2`             |      0 B |   4.4 MB |   +4.4 MB |
| `cafesat.sat.Vector[]`                            |      0 B |   4.3 MB |   +4.3 MB |
| `cafesat.sat.Vector`                              |      0 B |   4.2 MB |   +4.2 MB |
| `cafesat.asts.core.Trees$ConnectiveApplication`   |      0 B |   3.9 MB |   +3.9 MB |
| `cafesat.sat.Solver$Clause`                       |      0 B |   3.6 MB |   +3.6 MB |
| `cafesat.sat.Solver$Clause[]`                     |      0 B |   3.4 MB |   +3.4 MB |
| `int[]`                                           | 875.9 KB |   2.6 MB |   +1.8 MB |
| `scala.collection.immutable.Set$Set3`             |      0 B |   1.2 MB |   +1.2 MB |
| `cafesat.asts.core.Trees$ConnectiveSymbol`        |      0 B | 825.9 KB | +825.9 KB |
| `cafesat.asts.core.Trees$PredicateApplication`    |      0 B | 463.3 KB | +463.3 KB |
| `java.lang.Integer`                               |   5.1 KB | 153.4 KB | +148.2 KB |
| `cafesat.api.Formulas$Formula`                    |      0 B | 139.2 KB | +139.2 KB |
| `cafesat.common.FixedIntStack`                    |      0 B |  71.6 KB |  +71.6 KB |
| `java.lang.ref.SoftReference`                     |   1.1 MB |   1.2 MB |  +64.1 KB |
| `java.util.jar.Manifest`                          |   1.1 MB |   1.1 MB |  +62.6 KB |
| `java.util.HashMap$Node`                          |   1.5 MB |   1.5 MB |  +61.8 KB |
| `java.util.HashMap`                               |   1.5 MB |   1.6 MB |  +61.7 KB |
| `java.util.HashMap$Node[]`                        |   1.5 MB |   1.6 MB |  +61.7 KB |
| `java.util.jar.JarFile`                           |   1.1 MB |   1.2 MB |  +60.4 KB |

### New Classes

| Class                                             |  r1 |       r2 |  Δ(r1→rN) |
| ------------------------------------------------- | --: | -------: | --------: |
| `scala.collection.immutable.BitmapIndexedSetNode` | 0 B |   8.4 MB |   +8.4 MB |
| `scala.collection.immutable.HashSet`              | 0 B |   8.0 MB |   +8.0 MB |
| `cafesat.sat.Literal`                             | 0 B |   4.8 MB |   +4.8 MB |
| `cafesat.sat.Solver`                              | 0 B |   4.6 MB |   +4.6 MB |
| `scala.collection.immutable.Set$Set2`             | 0 B |   4.4 MB |   +4.4 MB |
| `cafesat.sat.Vector[]`                            | 0 B |   4.3 MB |   +4.3 MB |
| `cafesat.sat.Vector`                              | 0 B |   4.2 MB |   +4.2 MB |
| `cafesat.asts.core.Trees$ConnectiveApplication`   | 0 B |   3.9 MB |   +3.9 MB |
| `cafesat.sat.Solver$Clause`                       | 0 B |   3.6 MB |   +3.6 MB |
| `cafesat.sat.Solver$Clause[]`                     | 0 B |   3.4 MB |   +3.4 MB |
| `scala.collection.immutable.Set$Set3`             | 0 B |   1.2 MB |   +1.2 MB |
| `cafesat.asts.core.Trees$ConnectiveSymbol`        | 0 B | 825.9 KB | +825.9 KB |
| `cafesat.asts.core.Trees$PredicateApplication`    | 0 B | 463.3 KB | +463.3 KB |
| `cafesat.api.Formulas$Formula`                    | 0 B | 139.2 KB | +139.2 KB |
| `cafesat.common.FixedIntStack`                    | 0 B |  71.6 KB |  +71.6 KB |
| `scala.collection.immutable.BitmapIndexedMapNode` | 0 B |  51.7 KB |  +51.7 KB |
| `cafesat.asts.core.Trees$PredicateSymbol`         | 0 B |  51.4 KB |  +51.4 KB |
| `scala.collection.immutable.Vector3`              | 0 B |  36.6 KB |  +36.6 KB |
| `cafesat.api.Formulas$Formula[]`                  | 0 B |  35.1 KB |  +35.1 KB |
| `cafesat.api.Formulas$PropVar[][][]`              | 0 B |  16.4 KB |  +16.4 KB |
| `cafesat.api.Formulas$PropVar[][]`                | 0 B |  16.3 KB |  +16.3 KB |
| `cafesat.api.Formulas$PropVar[]`                  | 0 B |  15.8 KB |  +15.8 KB |
| `org.renaissance.scala.sat.Solver$`               | 0 B |  13.3 KB |  +13.3 KB |
| `cafesat.api.Formulas$PropVar`                    | 0 B |  11.4 KB |  +11.4 KB |
| `cafesat.asts.fol.Manip$`                         | 0 B |   4.8 KB |   +4.8 KB |

### Removed Classes

| Class                                                                 |       r1 |  r2 |  Δ(r1→rN) |
| --------------------------------------------------------------------- | -------: | --: | --------: |
| `scala.concurrent.stm.ccstm.InTxnImpl`                                |   3.7 MB | 0 B |   −3.7 MB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$PhilosopherThread` |   1.0 MB | 0 B |   −1.0 MB |
| `scala.concurrent.stm.ccstm.Handle[]`                                 |   1.0 MB | 0 B |   −1.0 MB |
| `scala.concurrent.stm.skel.CallbackList`                              | 417.2 KB | 0 B | −417.2 KB |
| `scala.Function1[]`                                                   | 399.1 KB | 0 B | −399.1 KB |
| `org.renaissance.scala.stm.RealityShowPhilosophers$CameraThread`      | 157.5 KB | 0 B | −157.5 KB |
| `scala.concurrent.stm.skel.SimpleRandom$`                             |  64.5 KB | 0 B |  −64.5 KB |
| `scala.Console$`                                                      |  24.6 KB | 0 B |  −24.6 KB |
| `scala.util.DynamicVariable`                                          |  24.5 KB | 0 B |  −24.5 KB |
| `java.io.BufferedReader`                                              |  24.4 KB | 0 B |  −24.4 KB |
| `java.util.concurrent.atomic.AtomicReferenceArray`                    |  13.2 KB | 0 B |  −13.2 KB |
| `scala.concurrent.stm.ccstm.TxnLevelImpl`                             |  13.1 KB | 0 B |  −13.1 KB |
| `scala.collection.GenIterable`                                        |  11.2 KB | 0 B |  −11.2 KB |
| `scala.collection.GenTraversable`                                     |  10.5 KB | 0 B |  −10.5 KB |
| `scala.concurrent.stm.ccstm.WakeupManager`                            |   8.7 KB | 0 B |   −8.7 KB |
| `java.io.InputStreamReader`                                           |   8.3 KB | 0 B |   −8.3 KB |
| `scala.concurrent.stm.ccstm.TxnSlotManager`                           |   8.3 KB | 0 B |   −8.3 KB |
| `sun.nio.cs.StreamDecoder`                                            |   8.3 KB | 0 B |   −8.3 KB |
| `scala.concurrent.stm.ccstm.RetrySet`                                 |   5.4 KB | 0 B |   −5.4 KB |
| `scala.collection.immutable.StringLike`                               |   4.3 KB | 0 B |   −4.3 KB |
| `java.util.concurrent.atomic.AtomicLongArray`                         |   4.3 KB | 0 B |   −4.3 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$GenericRef`                     |   4.0 KB | 0 B |   −4.0 KB |
| `scala.collection.mutable.ResizableArray`                             |   3.3 KB | 0 B |   −3.3 KB |
| `scala.concurrent.stm.ccstm.CCSTMRefs$IntRef`                         |   3.0 KB | 0 B |   −3.0 KB |
| `scala.collection.MapLike`                                            |   2.5 KB | 0 B |   −2.5 KB |

### New / Grown Leak Suspects

| Suspect            |  r1 |      r2 | Δ(r1→rN) | New? |
| ------------------ | --: | ------: | -------: | ---- |
| `java.lang.Thread` | 0 B | 22.9 MB | +22.9 MB | yes  |
| `java.lang.Class`  | 0 B |  3.5 MB |  +3.5 MB | yes  |

### Shrunk Leak Suspects

No leak suspect shrank in the current dump.

### Disappeared Leak Suspects (resolved)

_Informational: these were flagged in an earlier dump but are gone from the current one — a fixed or transient issue, not a current problem. Listed last for that reason._

| Suspect                                |     r1 |  r2 | Δ(r1→rN) |
| -------------------------------------- | -----: | --: | -------: |
| `scala.concurrent.stm.ccstm.InTxnImpl` | 2.7 MB | 0 B |  −2.7 MB |
| `scala.runtime.LazyVals$`              | 2.5 MB | 0 B |  −2.5 MB |
| `java.util.zip.ZipFile$Source`         | 1.3 MB | 0 B |  −1.3 MB |

