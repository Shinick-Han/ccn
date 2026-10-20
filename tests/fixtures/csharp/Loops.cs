// `for`, `while`, and `foreach`.
using System.Collections.Generic;

namespace Fixtures
{
    static class Loops
    {
        public static int Sum(List<int> values)
        {
            int total = 0;
            for (int i = 0; i < values.Count; i++)
            {
                total += values[i];
            }
            return total;
        }

        public static int Countdown(int start)
        {
            int steps = 0;
            while (start > 0)
            {
                start--;
                steps++;
            }
            return steps;
        }

        public static int Total(List<int> values)
        {
            int result = 0;
            foreach (int value in values)
            {
                result += value;
            }
            return result;
        }
    }
}
