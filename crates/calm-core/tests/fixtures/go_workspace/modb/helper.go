package main

// Greet — same shape as moda/helper.go's, in its own module, so the live
// test can assert both modules' edges independently: if per-module
// `sub_root` rebasing were broken (e.g. both modules accidentally sharing
// one rebase prefix), one of these two assertions would land on the wrong
// path and fail even if the indexer itself ran fine.
func Greet(name string) string {
	return "Hello from modb, " + name
}
