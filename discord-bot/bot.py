import os
import logging

import aiohttp
import discord
from discord.ext import tasks
from dotenv import load_dotenv

load_dotenv()

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")
log = logging.getLogger("stats-bot")

GITHUB_REPO = "yvgude/lean-ctx"
STATS_API = "https://leanctx.com/api/stats"

UPDATE_INTERVAL_MINUTES = 10


def format_number(n: int) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 10_000:
        return f"{n / 1_000:.1f}k"
    return f"{n:,}".replace(",", "'")


class StatsBot(discord.Client):
    def __init__(self):
        intents = discord.Intents.default()
        super().__init__(intents=intents)
        self.guild_id = int(os.environ["GUILD_ID"])
        self.ch_stars = int(os.environ["CHANNEL_STARS"])
        self.ch_installs = int(os.environ["CHANNEL_INSTALLS"])
        self.session: aiohttp.ClientSession | None = None

    async def setup_hook(self):
        self.session = aiohttp.ClientSession()
        self.update_stats.start()

    async def on_ready(self):
        log.info("Bot online as %s", self.user)

    async def close(self):
        if self.session:
            await self.session.close()
        await super().close()

    async def fetch_stats(self) -> tuple[int, int]:
        """Fetch stars and total installs from leanctx.com/api/stats."""
        async with self.session.get(STATS_API) as resp:
            if resp.status != 200:
                log.warning("Stats API %s: %s", resp.status, await resp.text())
                return -1, -1
            data = await resp.json()
            return data.get("stars", -1), data.get("installs", -1)

    async def _rename(self, channel_id: int, name: str):
        guild = self.get_guild(self.guild_id)
        if not guild:
            log.error("Guild %s not found", self.guild_id)
            return
        channel = guild.get_channel(channel_id)
        if not channel:
            log.error("Channel %s not found", channel_id)
            return
        if channel.name == name:
            return
        try:
            await channel.edit(name=name)
            log.info("Updated: %s", name)
        except discord.HTTPException as e:
            log.error("Failed to rename channel %s: %s", channel_id, e)

    @tasks.loop(minutes=UPDATE_INTERVAL_MINUTES)
    async def update_stats(self):
        log.info("Fetching stats...")

        stars, installs = await self.fetch_stats()

        if stars >= 0:
            await self._rename(self.ch_stars, f"⭐ Stars: {format_number(stars)}")
        if installs >= 0:
            await self._rename(self.ch_installs, f"📦 Installs: {format_number(installs)}")

        log.info("Stats update complete.")

    @update_stats.before_loop
    async def before_update(self):
        await self.wait_until_ready()


def main():
    bot = StatsBot()
    bot.run(os.environ["DISCORD_BOT_TOKEN"])


if __name__ == "__main__":
    main()
