// Package fixtures: compound predicates.
package fixtures

func CanPublish(isAdmin bool, approved bool) bool {
	return isAdmin && approved
}

func ShouldRetry(attempts int, fatal bool) bool {
	return attempts < 3 || fatal
}

func Mixed(a bool, b bool, c bool) bool {
	return a && (b || c)
}

func Clamped(value int, low int, high int) int {
	if value < low || value > high {
		return low
	}
	return value
}
