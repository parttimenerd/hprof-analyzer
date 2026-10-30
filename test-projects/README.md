Problems
--------

Clearly the Node class is the class that costs a lot of memory, but one cannot discover it, allow to dig down
-> the class dominator graph needs major fixing
also there should be a table and views for the overall memory usage of classes, not just the JDK classes, and also for the non-JDK classes
(maybe even a dig down view, but maybe that is the dominator graph)
Make all plots interactive
improve the rules for such recursive classes
maybe also have a view that focuses on the biggest non-JDK classes
help discover such scenarios, maybe even recursive clusters

Same with Simple -> the Simple class is important , but also not shown in "Who Holds This Class?", this is really bad

idea: create many sample projects and verify that the analysis works on them, maybe even create a test suite for this

create a proper test suite that makes it easy to create new testing scenarios, maybe use the "heap" sub commands for this




"CriticalHeadline Retainer: java.lang.Thread (a single object) retains 3.8 MB (79.4% of reachable heap). See Leak Suspects.
CriticalConcentration: highly concentrated — java.lang.Thread (a single object) holds 79.4% of the heap; freeing this object would reclaim most of the heap. See Leak Suspects.
WarningDominant GC-Root Type: 79.4% of the heap is held by "Thread" roots — the GC Roots by Type table shows the per-class breakdown. See System Overview.
" is kind of pointless?
back button not working

fullanalysis in webversion should trigger --collections and should showcase in the empty-array-list dump that
there is an empty arraylist with a capacity of 1,000,000 and that it is the only object in the heap




The mean idea here is to create many small test cases/examples the check all common scenarios for memory leaks
and for huge memory usage, and then verify that the analysis works on them (both that the triage catches things and
that the other parts of the generated report are correct and helpful). This test suite should also ensure that I can
easily add new test scenerios related to stakeholders/bug reports, so there are less regressions in the future.


Include also things like:
- Class loaded by multiple classloaders (spamming metaspace)
- Classloader leaks (classloaders that are not GCed)
- ThreadLocal on pooled thread
- excessive number of threads
- Direct buffer / off-heap	(see Palmr/java-off-heap-leak-example, iranna90/MemoryLeakDemo on github and create small version)
- Thread / executor leak	Thread pools created but never shut down; threads accumulate






