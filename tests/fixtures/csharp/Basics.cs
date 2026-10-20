// Basic branching shapes: the base-1 guard and plain conditionals.
namespace Fixtures
{
    static class Basics
    {
        public static int Identity(int value)
        {
            return value;
        }

        public static int Pick(bool flag)
        {
            if (flag)
            {
                return 1;
            }
            return 0;
        }

        public static string Grade(int score)
        {
            if (score >= 90)
            {
                return "A";
            }
            else if (score >= 80)
            {
                return "B";
            }
            else
            {
                return "C";
            }
        }
    }
}
