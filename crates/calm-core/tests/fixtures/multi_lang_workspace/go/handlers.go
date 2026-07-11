package main

// AlphaHandler and BetaHandler both define a Process method with the same
// name — dispatch below type-switches on an interface value, so a syntactic
// resolver can't tell which Process a given call site targets without real
// type information (the Go analogue of Kotlin's `is X ->` smart cast).
type AlphaHandler struct{}

func (AlphaHandler) Process() string {
	return "alpha"
}

type BetaHandler struct{}

func (BetaHandler) Process() string {
	return "beta"
}

func Dispatch(useAlpha bool) string {
	var v interface{}
	if useAlpha {
		v = AlphaHandler{}
	} else {
		v = BetaHandler{}
	}
	return route(v)
}

func route(v interface{}) string {
	switch h := v.(type) {
	case AlphaHandler:
		return h.Process()
	case BetaHandler:
		return h.Process()
	default:
		return "unknown"
	}
}
