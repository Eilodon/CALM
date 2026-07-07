package main

// Greet is called from main.go with no import — same-package resolution.
func Greet(name string) string {
	return "Hello, " + name
}
