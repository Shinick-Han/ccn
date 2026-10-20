// A two-section switch (no default, so section counts stay unambiguous),
// plus try/catch.
using System;

namespace Fixtures
{
    static class Switches
    {
        public static string Route(string method)
        {
            switch (method)
            {
                case "GET":
                    return "read";
                case "POST":
                    return "write";
            }
            return "unknown";
        }

        public static int SafeDivide(int a, int b)
        {
            try
            {
                return a / b;
            }
            catch (DivideByZeroException)
            {
                return 0;
            }
        }
    }
}
