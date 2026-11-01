# The default orders

When you leave the region order or the video order of a selection profile empty, for the whole profile or for one
system, selection uses the default order. The workbench lists the order in full under the setting: open "The default
order". `GET /api/v1/vocabulary` returns it as `default_region_order` and `default_video_order`.

## What they are

The default region order puts USA first, then the regions whose releases are most often in English, then the rest of
the world. At the pinned engine it is:

USA, World, Canada, Europe, UK, Australia, New Zealand, Singapore, Ireland, Japan, Asia, Thailand, Spain, Mexico,
Argentina, Peru, Latin America, Brazil, Portugal, France, Belgium, Netherlands, Germany, Austria, Italy, Switzerland,
Hong Kong, China, Taiwan, Korea, Russia, Ukraine, Estonia, Poland, Latvia, Lithuania, Denmark, Norway, Sweden,
Scandinavia, Finland, Iceland, Hungary, Czech, Greece, Macedonia, India, South Africa, Israel, Slovakia, Turkey,
Croatia, Slovenia, United Arab Emirates, Bulgaria, Romania, Albania, Serbia, Indonesia, Unknown.

The default video order is NTSC, PAL, PAL 60Hz, MPAL, SECAM.

So the defaults suit English speakers. The workbench and the API always show the order your selection actually uses,
so trust them over this page if the two ever differ.

## Where they come from

Selection is derived from [Retool](https://github.com/unexpectedpanda/retool), and the default orders are Retool's
own: `defaultRegionOrder` and `defaultVideoOrder` in its `internal-config.json`, in file order. Retool's "Use default
order for English speakers" button fills in the same region order. Title Select uses the orders as its pinned copy of
Retool ships them, and does not change them.

## Choosing your own order

In a selection profile, choose "Start from the default order" to copy the default into the setting, then move, add
or remove regions. A region you leave out of your own order is not selected. "Reset to the default order" empties the
setting again, so the default applies.

On a system's tab, "Reset to defaults for this system" sets every setting for that system to the defaults, whatever
your global settings say; your global overrides still apply, and your choices stay. "Use global settings for this
system" goes back to your global settings instead.
