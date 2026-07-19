// Package structural is the Go structural-extraction fixture.
// It exercises Unicode, declarations, calls, imports, comments, and strings.
package structural

import "fmt"

type Greeter struct {
	Prefix string
}

// Greet returns a localized greeting.
func (greeter Greeter) Greet(name string) string {
	text := "Hello 🌍"
	fmt.Println(name)
	return greeter.Prefix + text
}

const meaning = 42
var defaultName = `world`
