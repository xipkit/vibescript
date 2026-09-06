package parser

import "github.com/mgomes/vibescript/internal/ast"

// MemberReceiverFor parses source and returns the receiver expression of the
// member access whose property is probe, together with the parameters of the
// function whose body encloses it.
//
// Editor tooling needs the receiver at the cursor, and a bare trailing dot --
// exactly what the buffer holds when completion fires -- is a syntax error that
// yields no member node at all. The caller therefore splices a probe name in at
// the cursor and asks for that member here.
//
// The receiver is captured while parsing rather than found afterwards: the
// parser holds it at the moment it builds the member access, so recovering it
// later would mean walking the tree, and this package exposes no traversal.
// Parse errors elsewhere in the document are ignored, since a buffer under
// edit rarely parses cleanly as a whole.
func MemberReceiverFor(source, probe string) (ast.Expression, []ast.Param, bool) {
	p := newParser(source)
	p.memberReceiverProbe = probe
	p.parseProgram()
	if p.memberReceiver == nil || p.nestingError != nil {
		return nil, nil, false
	}
	return p.memberReceiver, p.memberReceiverParams, true
}
