// Package fixtures: channel coordination.
package fixtures

func Receive(ch chan int) int {
	select {
	case value := <-ch:
		return value
	case <-ch:
		return 0
	}
}

func Drain(ch chan int, done chan struct{}) int {
	total := 0
	for {
		select {
		case value := <-ch:
			total += value
		case <-done:
			return total
		}
	}
}

func Spawn(ch chan int, value int) {
	go func() {
		if value > 0 {
			ch <- value
		}
	}()
}
