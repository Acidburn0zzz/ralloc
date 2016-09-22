use prelude::*;

use core::{cmp, mem};

use arena::Arena;
use random;

struct Pool {
    head: Node,
    arena: Arena<Node>,
}

impl Pool {
    fn search(&mut self, block: Block) -> Seek {
        log!(DEBUG, "Searching the block pool for block {:?}...", block);

        // We start by an uninitialized value, which we fill out.
        let mut seek = unsafe { mem::uninitialized() };

        // The outline of the algorithm is this: We start by shortcutting from the top level until
        // we overshoot, then we repeat on the next level starting at the last non-overshot
        // shortcut from the previous level.

        // If we look for 8, we start in the top level and follow until we hit 9.
        //     # ~~~~~~~~~~~~~~~~~~> [6] --- overshoot ----> [9] -----------> NIL
        //     # ------------------> [6] --> [7] ----------> [9] -----------> NIL
        //     # ----------> [5] --> [6] --> [7] ----------> [9] --> [10] --> NIL
        //     # --> [1] --> [5] --> [6] --> [7] --> [8] --> [9] --> [10] --> NIL

        // Start at the highest (least dense) level.
        let mut iter = self.head.shortcuts(shortcut::Level::max());
        // Go forward until we overshoot.
        while let Some(shortcut_taken) = iter.take_while(|x| x < block).last() {

            // Decrement the level.
            let shortcut::Level(lv) = iter.decrement_level();
            log!(INTERNAL, "Going from level {} to level {}.", lv, lv - 1);

            // Update the back look respectively.
            seek.back_look[lv] = shortcut_taken;

            // End the loop at the last level.
            if lv == 1 {
                // We decremented the level previously, and given that our old level is one, the
                // new level is zero.

                log!(INTERNAL, "We're at the last level now.");

                break;
            }
        }

        // We're now at the bottom layer, in which we will iterate over to find the last element,
        // below our needle.
        // FIXME: These unwraps can be eliminated, find an approach which does not significantly
        // increase CLOC of this function.
        seek.node = iter.unwrap_node().iter().take_while(|x| x < block).last().unwrap();

        seek.check();

        // Everything have been initialized, including all the back look cells (i.e. every level
        // have been visited).
        seek
    }
}




















// Here is a rare Ferris to cheer you up.
//          |
//        \ _ /
//      -= (_) =-
//        /   \         _\/_ _\/_
//          |           //o\ /o\\
//   _____ _ __ __ _______|____|___________
// =-=-_-__=_-= _=_=-=_,-'|_   |_  ()    ()
//  =- _=-=- -_=-=_,-     "-   "-   \/\/\/
//    =- =- -=.--"                  &_^^_&
//                                  \    /
// Don't share beach crab or it will lose
// its value.
