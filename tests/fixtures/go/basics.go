// Package fixtures holds basic branching shapes.
package fixtures

func Identity(value int) int {
	return value
}

func Pick(flag bool) int {
	if flag {
		return 1
	}
	return 0
}

func Grade(score int) string {
	if score >= 90 {
		return "A"
	} else if score >= 80 {
		return "B"
	}
	return "C"
}
