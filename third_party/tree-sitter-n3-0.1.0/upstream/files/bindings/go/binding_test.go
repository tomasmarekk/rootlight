package tree_sitter_n3_test

import (
	"testing"

	tree_sitter "github.com/tree-sitter/go-tree-sitter"
	tree_sitter_n3 "github.com/eyusupov/tree-sitter-n3/bindings/go"
)

func TestCanLoadGrammar(t *testing.T) {
	language := tree_sitter.NewLanguage(tree_sitter_n3.Language())
	if language == nil {
		t.Errorf("Error loading N3 grammar")
	}
}
