// Lambdas are their own scope: an `if` inside one belongs to the lambda,
// never to the method that creates it.
using System;
using System.Collections.Generic;

namespace Fixtures
{
    static class Lambdas
    {
        public static Func<int, int> Absolute()
        {
            return value =>
            {
                if (value < 0)
                {
                    return -value;
                }
                return value;
            };
        }

        public static int CountAbove(List<int> values, int limit)
        {
            Func<int, int> bump = value => value + 1;
            int count = 0;
            foreach (int value in values)
            {
                if (value > limit)
                {
                    count += bump(value);
                }
            }
            return count;
        }
    }
}
