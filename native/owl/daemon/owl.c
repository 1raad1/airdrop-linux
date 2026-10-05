/*
 * OWL: an open Apple Wireless Direct Link (AWDL) implementation
 * Copyright (C) 2018  The Open Wireless Link Project (https://owlink.org)
 * Copyright (C) 2018  Milan Stute
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#include <stdint.h>
#include <unistd.h>
#include <stdlib.h>
#include <signal.h>
#include <syslog.h>
#include <errno.h>
#include <ev.h>

#include "log.h"
#include "core.h"

#define DEFAULT_AWDL_DEVICE "awdl0"
#define FAILED_DUMP "failed.pcap"

static void daemonize() {
	pid_t pid;
	long x;

	/* Fork off the parent process */
	pid = fork();

	/* An error occurred */
	if (pid < 0)
		exit(EXIT_FAILURE);

	/* Success: Let the parent terminate */
	if (pid > 0)
		exit(EXIT_SUCCESS);

	/* On success: The child process becomes session leader */
	if (setsid() < 0)
		exit(EXIT_FAILURE);

	/* Catch, ignore and handle signals */
	/* TODO: Implement a working signal handler */
	signal(SIGCHLD, SIG_IGN);
	signal(SIGHUP, SIG_IGN);

	/* Fork off for the second time*/
	pid = fork();

	/* An error occurred */
	if (pid < 0)
		exit(EXIT_FAILURE);

	/* Success: Let the parent terminate */
	if (pid > 0)
		exit(EXIT_SUCCESS);

	/* Set new file permissions */
	umask(0);

	/* Change the working directory to the root directory */
	/* or another appropriated directory */
	chdir("/");

	/* Close all open file descriptors */
	for (x = sysconf(_SC_OPEN_MAX); x >= 0; x--) {
		close(x);
	}

	/* Open the log file */
	openlog("owl", LOG_PID, LOG_DAEMON);
}

int main(int argc, char *argv[]) {
	int c;
	int daemon = 0;
	int dump = 0;
	int chan_num = 6;
	struct awdl_chan chan;
	int log_level = LOG_INFO;
	int filter_rssi = 1;
	int no_monitor_mode = 0;
	enum awdl_chanseq_strategy strategy = AWDL_CHANSEQ_PIN;
	int widen_max = 4;

	char wlan[PATH_MAX] = "";
	char host[IFNAMSIZ] = DEFAULT_AWDL_DEVICE;

	struct ev_loop *loop;

	struct daemon_state state;

	while ((c = getopt(argc, argv, "Dc:dvi:h:a:t:fNS:W:")) != -1) {
		switch (c) {
			case 'S':
				/* How to derive our channel sequence from a peer's. Exposed so a
				 * single test session can A/B all three against the same phone --
				 * the only way any of this gets settled. See channel.h. */
				if (!strcmp(optarg, "pin"))
					strategy = AWDL_CHANSEQ_PIN;
				else if (!strcmp(optarg, "rotate"))
					strategy = AWDL_CHANSEQ_ROTATE;
				else if (!strcmp(optarg, "verbatim"))
					strategy = AWDL_CHANSEQ_VERBATIM;
				else if (!strcmp(optarg, "widen"))
					strategy = AWDL_CHANSEQ_WIDEN;
				else if (!strcmp(optarg, "intersect"))
					strategy = AWDL_CHANSEQ_INTERSECT;
				else {
					log_error("Unknown channel sequence strategy '%s' "
					          "(use verbatim, widen, intersect, rotate, or pin)", optarg);
					return EXIT_FAILURE;
				}
				break;
			case 'W':
				/* How many of the peer's empty slots -S widen may fill. */
				widen_max = atoi(optarg);
				if (widen_max < 0 || widen_max > AWDL_CHANSEQ_LENGTH) {
					log_error("-W takes 0..%d", AWDL_CHANSEQ_LENGTH);
					return EXIT_FAILURE;
				}
				break;
			case 'D':
				daemon = 1;
				break;
			case 'c':
				chan_num = atoi(optarg);
				break;
			case 'd':
				dump = 1;
				break;
			case 'f':
				filter_rssi = 0;
				break;
			case 'v':
				if (log_level < LOG_TRACE)
					log_level++; /* increase log level with every occurence of option */
				break;
			case 'i':
				strcpy(wlan, optarg);
				break;
			case 'h':
				strcpy(host, optarg);
				break;
			case 'N':
				no_monitor_mode = 1;
				break;
			case '?':
				if (optopt == 'i')
					fprintf(stderr, "Option -%c needs to specify a wireless interface.\n", optopt);
				return EXIT_FAILURE;
			default:
				abort();
		}
	}

	log_set_level(log_level);

	printf("              .oOXWMMMMWXOx:\n"
	       "         .oOOOx:'''''''''''':OOOx:\n"
	       "      oXOo'      ........      ':OXx.\n"
	       "           .oOOO''''''''''OOOo.\n"
	       "        oXOo'                'oOO:\n"
	       "             :oOOOOXXXXOOOOo:.\n"
	       "          oXO:'            ':OXo\n"
	       "              .:xOXXXXXXOx:.\n"
	       "          .xXMMMMMMMMMMMMMMMMXx.\n"
	       "  'XWWWWWWMMMMMMMMMMMMMMMMMMMMMMWWWWWWX'\n"
	       "    oWMMMMMMMMMMMMMMMMMMMMMMMMMMMMMMWo\n"
	       "     OMMMMMMWWMMMMMMMMMMMMMMWWWMMMMMO\n"
	       "    OMMWx'      'xWMMMMWx'      'oXMMO\n"
	       "   :MW:            oMMx            'WM:\n"
	       "   XM'    .xOOo.    :o     .xOOo.    WX\n"
	       "   WX    :MMMMMX          :MMMMMX    xW\n"
	       "   XW    'WMMMMX   .xx.   'WMMMWX    XX\n"
	       "   'Wx    'xWMx'   OMMO    'xWMx'   xM'\n"
	       "    'XX:           'XX'           :XX'\n"
	       "      'xXOx:..................:xXWx'\n"
	       "         'xXMMMMMMMMMMMMMMMMMMWO'\n"
	       "\n"
	       "            Open Wireless Link\n"
	       "\n"
	       "            https://owlink.org\n"
	       "\n");

	if (daemon)
		daemonize();

	/* THE CHANNEL SET, PLACE 3 OF 3, and the one with the final say: a channel
	 * missing here makes owl exit right after printing its banner, which reads
	 * downstream as "awdl0 never appeared" rather than as a rejected channel.
	 * The two callers that gate on this set - airdrop-mt7921's
	 * daemon/airdrop-helper (`go-up` and `owl-start`) and its daemon/airdropd
	 * (GO_CHANNELS) - must be kept in step with it. */
	switch (chan_num) {
		case 6:
			chan = CHAN_OPCLASS_6;
			break;
		case 36:
			chan = CHAN_OPCLASS_36;
			break;
		case 44:
			chan = CHAN_OPCLASS_44;
			break;
		case 149:
			chan = CHAN_OPCLASS_149;
			break;
		default:
			log_error("Unsupported channel %d (use 6, 36, 44, or 149)", chan_num);
			return EXIT_FAILURE;
	}

	if (!*wlan) {
		log_error("No interface specified");
		return EXIT_FAILURE;
	}

	state.io.wlan_no_monitor_mode = no_monitor_mode;

	if (awdl_init(&state, wlan, host, chan, dump ? FAILED_DUMP : 0) < 0) {
		log_error("could not initialize core");
		return EXIT_FAILURE;
	}
	state.awdl_state.filter_rssi = filter_rssi;
	state.awdl_state.channel.strategy = strategy;
	state.awdl_state.channel.widen_max = widen_max;
	log_info("channel sequence strategy: %s",
	         strategy == AWDL_CHANSEQ_PIN ? "pin" :
	         strategy == AWDL_CHANSEQ_ROTATE ? "rotate" :
	         strategy == AWDL_CHANSEQ_WIDEN ? "widen" :
	         strategy == AWDL_CHANSEQ_INTERSECT ? "intersect" : "verbatim");
	if (strategy == AWDL_CHANSEQ_WIDEN)
		log_info("widening by at most %d slot(s)", widen_max);

	if (state.io.wlan_ifindex)
		log_info("WLAN device: %s (addr %s)", state.io.wlan_ifname, ether_ntoa(&state.io.if_ether_addr));
	if (state.io.host_ifindex)
		log_info("Host device: %s", state.io.host_ifname);

	loop = EV_DEFAULT;

	awdl_schedule(loop, &state);

	ev_run(loop, 0);

	awdl_free(&state);

	return EXIT_SUCCESS;
}
