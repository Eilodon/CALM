package main

// Greet is called from main.go with no import — same-package resolution,
// same shape as multi_lang_workspace/go's single-module fixture. Deliberately
// NOT a cross-module call into modb — this fixture's job is proving
// go.work module *enumeration* and per-module `sub_root` path rebasing work
// correctly, not exercising Go's own cross-module resolution semantics
// (already scip-go's responsibility, unrelated to this codebase).
func Greet(name string) string {
	return "Hello from moda, " + name
}
